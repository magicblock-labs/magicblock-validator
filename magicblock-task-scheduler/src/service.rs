use std::{
    collections::HashMap,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use futures_util::StreamExt;
use magicblock_config::config::TaskSchedulerConfig;
use magicblock_core::{
    coordination_mode::CoordinationMode, link::transactions::ScheduledTasksRx,
};
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    args::{CancelTaskRequest, TaskRequest},
    instruction_utils::InstructionUtils,
    validator::{validator_authority, validator_authority_id},
};
use solana_instruction::Instruction;
use solana_message::Message;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_transaction::Transaction;
use tokio::{
    select,
    task::JoinHandle,
    time::{interval, Duration, MissedTickBehavior},
};
use tokio_util::{
    sync::CancellationToken,
    time::{delay_queue::Key, DelayQueue},
};
use tracing::*;

use crate::{
    db::{DbTask, SchedulerDatabase},
    errors::{TaskSchedulerError, TaskSchedulerResult},
};

const MAX_TASK_EXECUTION_RETRIES: u32 = 10;
const TASK_EXECUTION_RETRY_BASE_DELAY: Duration = Duration::from_millis(100);
const TASK_EXECUTION_RETRY_MAX_DELAY: Duration = Duration::from_secs(5);

pub struct TaskSchedulerService {
    /// Database for persisting tasks
    db: SchedulerDatabase,
    /// RPC client used to send transactions
    rpc_client: RpcClient,
    /// Used to receive scheduled tasks from the transaction executor
    scheduled_tasks: ScheduledTasksRx,
    /// Provides latest blockhash for signing transactions
    block: LatestBlock,
    /// Queue of tasks to execute
    task_queue: DelayQueue<DbTask>,
    /// Map of task IDs to their corresponding keys in the task queue
    task_queue_keys: HashMap<i64, Key>,
    /// Number of consecutive failed execution attempts for each task
    task_execution_retries: HashMap<i64, u32>,
    /// Counter used to make each transaction unique
    tx_counter: AtomicU64,
    /// Token used to cancel the task scheduler
    token: CancellationToken,
    /// Minimum interval between task executions
    min_interval: Duration,
    /// How long failed task and scheduling records are retained.
    failed_task_retention: Duration,
    /// How often failed task and scheduling records are cleaned up.
    failed_task_cleanup_interval: Duration,
    /// Slot interval of the validator
    slot_interval: Duration,
}

enum ProcessingOutcome {
    Success,
    Recoverable(Box<TaskSchedulerError>),
}

// SAFETY: TaskSchedulerService is moved into a single Tokio task in `start()` and never cloned.
// It runs exclusively on that task's thread. All fields (SchedulerDatabase, TransactionSchedulerHandle,
// ScheduledTasksRx, LatestBlock, DelayQueue, HashMap, AtomicU64, CancellationToken) are Send+Sync,
// and the service maintains exclusive ownership throughout its lifetime.
unsafe impl Send for TaskSchedulerService {}
unsafe impl Sync for TaskSchedulerService {}
impl TaskSchedulerService {
    pub fn new(
        path: &Path,
        config: &TaskSchedulerConfig,
        rpc_url: String,
        scheduled_tasks: ScheduledTasksRx,
        block: LatestBlock,
        slot_interval: Duration,
        token: CancellationToken,
    ) -> TaskSchedulerResult<Self> {
        if config.reset {
            match std::fs::remove_file(path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    debug!("Database file not found, skip resetting");
                }
                Err(e) => {
                    warn!("Failed to remove database file: {}", e);
                    return Err(TaskSchedulerError::Io(e));
                }
            }
        }

        // Reschedule all persisted tasks
        let db = SchedulerDatabase::new(path)?;
        Ok(Self {
            db,
            rpc_client: RpcClient::new(rpc_url),
            scheduled_tasks,
            block,
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: AtomicU64::default(),
            token,
            min_interval: config.min_interval,
            failed_task_retention: config.failed_task_retention,
            failed_task_cleanup_interval: config.failed_task_cleanup_interval,
            slot_interval,
        })
    }

    pub async fn start(
        mut self,
    ) -> TaskSchedulerResult<JoinHandle<TaskSchedulerResult<()>>> {
        if self.is_primary_mode().await {
            self.load_persisted_tasks().await?;
            Ok(tokio::spawn(self.run()))
        } else {
            debug!("Task scheduler on standby mode does not start");
            Ok(tokio::spawn(async move { Ok(()) }))
        }
    }

    async fn load_persisted_tasks(&mut self) -> TaskSchedulerResult<()> {
        self.task_queue.clear();
        self.task_queue_keys.clear();
        self.task_execution_retries.clear();

        // Reschedule all tasks that are due
        let tasks = self.db.get_tasks().await?;
        let now = chrono::Utc::now().timestamp_millis();
        debug!(
            "Task scheduler starting at {} with {} tasks",
            now,
            tasks.len()
        );
        for task in tasks {
            debug!("Task: {:?}", task);
            if !is_valid_task_interval(task.execution_interval_millis)
                || task.executions_left <= 0
            {
                warn!(
                    "Task {} has an invalid parameters: (interval={}, executions_left={}). Skipping.",
                    task.id, task.execution_interval_millis, task.executions_left
                );
                self.db.remove_task(task.id).await?;
                continue;
            }

            let next_execution =
                task.last_execution_millis + task.execution_interval_millis;
            // Earliest reschedule time is 2 slot interval.
            // This avoids, scheduling before the first blockhash is produced on restart.
            let timeout = Duration::from_millis(
                next_execution
                    .saturating_sub(now)
                    .max(2 * self.slot_interval.as_millis() as i64)
                    as u64,
            );
            let task_id = task.id;
            let key = self.task_queue.insert(task, timeout);
            self.task_queue_keys.insert(task_id, key);
        }

        Ok(())
    }

    async fn process_request(
        &mut self,
        request: TaskRequest,
    ) -> TaskSchedulerResult<ProcessingOutcome> {
        match request {
            TaskRequest::Schedule(mut schedule_request) => {
                if !is_valid_task_interval(
                    schedule_request.execution_interval_millis,
                ) {
                    // If the interval is too large or zero, we don't schedule the task
                    return Ok(ProcessingOutcome::Success);
                }

                schedule_request.execution_interval_millis =
                    schedule_request.execution_interval_millis.clamp(
                        self.min_interval.as_millis() as i64,
                        u32::MAX as i64,
                    );

                if let Err(e) = self.register_task(&schedule_request).await {
                    self.db
                        .insert_failed_scheduling(
                            schedule_request.id,
                            format!("{:?}", e),
                        )
                        .await?;
                    error!(
                        "Failed to process schedule request {}: {}",
                        schedule_request.id, e
                    );

                    return Ok(ProcessingOutcome::Recoverable(Box::new(e)));
                }
            }
            TaskRequest::Cancel(cancel_request) => {
                if let Err(e) =
                    self.process_cancel_request(&cancel_request).await
                {
                    self.db
                        .insert_failed_scheduling(
                            cancel_request.task_id,
                            format!("{:?}", e),
                        )
                        .await?;
                    error!(
                        "Failed to process cancel request for task {}: {}",
                        cancel_request.task_id, e
                    );

                    return Ok(ProcessingOutcome::Recoverable(Box::new(e)));
                }
            }
        };

        Ok(ProcessingOutcome::Success)
    }

    async fn process_cancel_request(
        &mut self,
        cancel_request: &CancelTaskRequest,
    ) -> TaskSchedulerResult<()> {
        let Some(task) = self.db.get_task(cancel_request.task_id).await? else {
            // Task not found in the database, cleanup the queue
            self.remove_task_from_queue(cancel_request.task_id);
            self.task_execution_retries.remove(&cancel_request.task_id);
            return Ok(());
        };

        // Check if the task authority is the same as the cancel request authority
        if task.authority != cancel_request.authority {
            error!(
                "Task authority {} does not match cancel request authority {}",
                task.authority, cancel_request.authority
            );
            return Ok(());
        }

        // Remove task from queue and database
        self.unregister_task(cancel_request.task_id).await?;

        Ok(())
    }

    async fn execute_task(&mut self, task: &DbTask) -> TaskSchedulerResult<()> {
        let sig = self.process_transaction(task.instructions.clone()).await?;

        // TODO(Dodecahedr0x): we don't get any output directly at this point
        // we would have to fetch the transaction via its signature to see
        // if it succeeded or failed.
        // However that should not happen here, but on a separate task
        // If sending the transaction fails, the task is retried in run().
        debug!("Executed task {} with signature {}", task.id, sig);

        if task.executions_left > 1 {
            // Reschedule the task
            let new_task = DbTask {
                executions_left: task.executions_left - 1,
                ..task.clone()
            };
            let key = self.task_queue.insert(
                new_task,
                Duration::from_millis(task.execution_interval_millis as u64),
            );
            self.task_queue_keys.insert(task.id, key);
        }

        if task.executions_left <= 1 {
            self.db.remove_task(task.id).await?;
        } else {
            let current_time = chrono::Utc::now().timestamp_millis();
            self.db
                .update_task_after_execution(task.id, current_time)
                .await?;
        }

        Ok(())
    }

    pub async fn register_task(
        &mut self,
        task: impl Into<DbTask>,
    ) -> TaskSchedulerResult<()> {
        let task = task.into();

        // Check if the task already exists in the database
        if let Some(db_task) = self.db.get_task(task.id).await? {
            if db_task.authority != task.authority {
                return Err(TaskSchedulerError::UnauthorizedReplacing(
                    task.id,
                    db_task.authority.to_string(),
                    task.authority.to_string(),
                ));
            }
        }

        self.db.insert_task(&task).await?;
        self.remove_task_from_queue(task.id);
        self.task_execution_retries.remove(&task.id);
        let key = self
            .task_queue
            .insert(task.clone(), Duration::from_millis(0));
        self.task_queue_keys.insert(task.id, key);
        debug!("Registered task {} from context", task.id);

        Ok(())
    }

    pub async fn unregister_task(
        &mut self,
        task_id: i64,
    ) -> TaskSchedulerResult<()> {
        self.remove_task_from_queue(task_id);
        self.task_execution_retries.remove(&task_id);
        self.db.remove_task(task_id).await?;
        debug!("Removed task {} from database", task_id);

        Ok(())
    }

    pub async fn run(mut self) -> TaskSchedulerResult<()> {
        let mut failed_task_cleanup = interval(
            self.failed_task_cleanup_interval
                .max(Duration::from_millis(1)),
        );
        failed_task_cleanup.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            select! {
                Some(task) = self.task_queue.next() => {
                    let task = task.get_ref();
                    self.task_queue_keys.remove(&task.id);
                    match self.execute_task(task).await {
                        Ok(()) => {
                            self.task_execution_retries.remove(&task.id);
                        }
                        Err(e) => {
                            self.handle_failed_task_execution(task, e).await?;
                        }
                    }
                }
                Some(task) = self.scheduled_tasks.recv() => {
                    let id = task.id();
                    match self.process_request(task).await {
                        Ok(ProcessingOutcome::Success) => {}
                        Ok(ProcessingOutcome::Recoverable(e)) => {
                            warn!("Failed to process request ID={}: {e:?}", id);
                        }
                        Err(e) => {
                            error!("Failed to process request: {}", e);
                            return Err(e);
                        }
                    }
                }
                _ = failed_task_cleanup.tick() => {
                    let cutoff = chrono::Utc::now().timestamp_millis().saturating_sub(
                        self.failed_task_retention.as_millis().min(i64::MAX as u128) as i64,
                    );
                    let deleted =  match self.db.delete_failed_records_older_than(cutoff).await {
                        Ok(deleted) => deleted,
                        Err(e) => {
                            error!("Failed to cleanup old failed task records: {}", e);
                            continue;
                        }
                    };
                    if deleted > 0 {
                        debug!(
                            deleted,
                            cutoff_timestamp_millis = cutoff,
                            "Cleaned up old failed task records"
                        );
                    }
                }
                _ = self.token.cancelled() => {
                    break;
                }
            }
        }

        info!("TaskSchedulerService shutdown!");
        Ok(())
    }

    fn remove_task_from_queue(&mut self, task_id: i64) {
        if let Some(key) = self.task_queue_keys.remove(&task_id) {
            self.task_queue.remove(&key);
        }
    }

    async fn handle_failed_task_execution(
        &mut self,
        task: &DbTask,
        error: TaskSchedulerError,
    ) -> TaskSchedulerResult<()> {
        debug!("Failed to execute task {}: {}", task.id, error);

        if !is_retryable_task_execution_error(&error) {
            return self.record_failed_task(task, error).await;
        }

        let Some(retries) = ({
            let retries =
                self.task_execution_retries.entry(task.id).or_default();
            if *retries >= MAX_TASK_EXECUTION_RETRIES {
                None
            } else {
                *retries += 1;
                Some(*retries)
            }
        }) else {
            return self.record_failed_task(task, error).await;
        };
        let delay = self.task_execution_retry_delay(retries);
        debug!(
            task_id = task.id,
            retry = retries,
            max_retries = MAX_TASK_EXECUTION_RETRIES,
            delay_ms = delay.as_millis(),
            error = %error,
            "Retrying failed task execution"
        );

        let key = self.task_queue.insert(task.clone(), delay);
        self.task_queue_keys.insert(task.id, key);

        Ok(())
    }

    async fn record_failed_task(
        &mut self,
        task: &DbTask,
        error: TaskSchedulerError,
    ) -> TaskSchedulerResult<()> {
        self.db
            .move_task_to_failed(task.id, format!("{:?}", error))
            .await?;
        self.task_execution_retries.remove(&task.id);
        self.remove_task_from_queue(task.id);

        Ok(())
    }

    fn task_execution_retry_delay(&self, retry: u32) -> Duration {
        let multiplier = 1u32
            .checked_shl(retry.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.slot_interval
            .max(TASK_EXECUTION_RETRY_BASE_DELAY)
            .checked_mul(multiplier)
            .unwrap_or(TASK_EXECUTION_RETRY_MAX_DELAY)
            .min(TASK_EXECUTION_RETRY_MAX_DELAY)
    }

    async fn process_transaction(
        &self,
        instructions: Vec<Instruction>,
    ) -> TaskSchedulerResult<Signature> {
        let blockhash = self.block.load().blockhash;
        // Execute unsigned transactions
        // We prepend a noop instruction to make each transaction unique.
        let ixs = [
            InstructionUtils::noop_instruction(
                self.tx_counter.fetch_add(1, Ordering::Relaxed),
            ),
            InstructionUtils::execute_task_instruction(instructions),
        ];
        let tx = Transaction::new(
            &[validator_authority()],
            Message::new(&ixs, Some(&validator_authority_id())),
            blockhash,
        );

        Ok(self
            .rpc_client
            .send_transaction(&tx)
            .await
            .map_err(Box::new)?)
    }

    /// Waits until the coordination mode is not StartingUp.
    /// Should be fast because task scheduler is started after the ledger replay completes.
    async fn is_primary_mode(&self) -> bool {
        let mut mode = CoordinationMode::current();
        while mode == CoordinationMode::StartingUp {
            tokio::select! {
                _ = self.token.cancelled() => return false,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            mode = CoordinationMode::current();
        }
        mode == CoordinationMode::Primary
    }
}

fn is_valid_task_interval(interval: i64) -> bool {
    interval > 0 && interval < u32::MAX as i64
}

fn is_retryable_task_execution_error(error: &TaskSchedulerError) -> bool {
    // `process_transaction` maps Solana send and verification failures to Rpc.
    matches!(error, TaskSchedulerError::Rpc(_))
}

#[cfg(test)]
mod tests {
    use magicblock_core::coordination_mode::{
        switch_to_primary_mode, switch_to_replica_mode,
    };
    use magicblock_program::{
        args::ScheduleTaskRequest,
        validator::generate_validator_authority_if_needed,
    };
    use serial_test::serial;
    use solana_pubkey::Pubkey;
    use tokio::{sync::mpsc, time::timeout};

    use super::*;

    fn test_service(
        db: SchedulerDatabase,
        scheduled_tasks: ScheduledTasksRx,
    ) -> TaskSchedulerService {
        TaskSchedulerService {
            db,
            rpc_client: RpcClient::new("http://localhost:8899".to_string()),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: AtomicU64::default(),
            token: CancellationToken::new(),
            min_interval: Duration::from_millis(1000),
            failed_task_retention: Duration::from_secs(60),
            failed_task_cleanup_interval: Duration::from_secs(60),
            slot_interval: Duration::from_millis(1000),
            scheduled_tasks,
        }
    }

    #[serial]
    #[tokio::test]
    async fn test_schedule_invalid_tasks() {
        magicblock_core::logger::init_for_tests();
        generate_validator_authority_if_needed();

        let (tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();

        let service = test_service(db.clone(), rx);

        let handle = service.start().await.unwrap();

        // Invalid task interval
        tx.send(TaskRequest::Schedule(ScheduleTaskRequest {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: u32::MAX as i64,
            iterations: 1,
            instructions: vec![],
        }))
        .unwrap();
        // Valid task interval
        tx.send(TaskRequest::Schedule(ScheduleTaskRequest {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: u32::MAX as i64 - 1,
            iterations: 1,
            instructions: vec![],
        }))
        .unwrap();

        // After processing the requests, only one task stays in the DB
        timeout(Duration::from_secs(1), async move {
            loop {
                let tasks = db.get_tasks().await.unwrap();
                if tasks.len() > 1 {
                    return Err::<(), String>(format!(
                        "Tasks should be 1, got {}",
                        tasks.len()
                    ));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_err();

        handle.abort();
    }

    #[serial]
    #[tokio::test]
    async fn test_remove_invalid_tasks_on_startup() {
        magicblock_core::logger::init_for_tests();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        // Invalid task interval
        db.insert_task(&DbTask {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: u32::MAX as i64,
            executions_left: 1,
            last_execution_millis: chrono::Utc::now().timestamp_millis(),
            instructions: vec![],
        })
        .await
        .unwrap();
        // Valid task interval
        db.insert_task(&DbTask {
            id: 2,
            authority: Pubkey::new_unique(),
            execution_interval_millis: u32::MAX as i64 - 1,
            executions_left: 1,
            last_execution_millis: chrono::Utc::now().timestamp_millis(),
            instructions: vec![],
        })
        .await
        .unwrap();
        let service = test_service(db.clone(), rx);

        let handle = service.start().await.unwrap();

        // After starting, only one task should be in the database
        timeout(Duration::from_secs(1), async move {
            loop {
                let tasks = db.get_tasks().await?;
                if tasks.len() == 1 {
                    return Ok::<_, TaskSchedulerError>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap()
        .unwrap();
        handle.abort();
    }

    #[serial]
    #[tokio::test]
    async fn test_completed_tasks_are_removed_on_startup() {
        magicblock_core::logger::init_for_tests();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        db.insert_task(&DbTask {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: 50,
            executions_left: 0,
            last_execution_millis: chrono::Utc::now().timestamp_millis(),
            instructions: vec![],
        })
        .await
        .unwrap();
        db.insert_task(&DbTask {
            id: 2,
            authority: Pubkey::new_unique(),
            execution_interval_millis: 50,
            executions_left: 1,
            last_execution_millis: chrono::Utc::now().timestamp_millis(),
            instructions: vec![],
        })
        .await
        .unwrap();

        let mut service = test_service(db.clone(), rx);
        service.min_interval = Duration::from_millis(10);

        let handle = service.start().await.unwrap();

        timeout(Duration::from_secs(1), async move {
            loop {
                let tasks = db.get_task_ids().await?;
                if tasks == vec![2] {
                    return Ok::<_, TaskSchedulerError>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap()
        .unwrap();
        handle.abort();
    }

    #[serial]
    #[tokio::test]
    async fn test_failed_records_are_cleaned_up_periodically() {
        magicblock_core::logger::init_for_tests();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        db.insert_failed_scheduling(1, "schedule failed".to_string())
            .await
            .unwrap();
        db.insert_failed_task(2, "task failed".to_string())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;

        let mut service = test_service(db.clone(), rx);
        service.failed_task_retention = Duration::from_millis(1);
        service.failed_task_cleanup_interval = Duration::from_millis(5);

        let handle = service.start().await.unwrap();

        timeout(Duration::from_secs(1), async move {
            loop {
                if db.get_failed_schedulings().await?.is_empty()
                    && db.get_failed_tasks().await?.is_empty()
                {
                    return Ok::<_, TaskSchedulerError>(());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
        .unwrap();
        handle.abort();
    }

    #[serial]
    #[tokio::test]
    async fn test_task_scheduler_does_not_start_on_standby_mode() {
        magicblock_core::logger::init_for_tests();
        switch_to_replica_mode();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        let service = test_service(db.clone(), rx);
        let handle = service.start().await.unwrap();

        switch_to_primary_mode();

        // Handle should join immediately because it's in standby mode
        timeout(Duration::from_secs(1), handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
