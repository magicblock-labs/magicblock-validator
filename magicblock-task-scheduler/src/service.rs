use std::{
    collections::HashMap,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use futures_util::StreamExt;
use magicblock_config::config::TaskSchedulerConfig;
use magicblock_core::link::transactions::ScheduledTasksRx;
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    args::{CancelTaskRequest, TaskRequest},
    instruction_utils::InstructionUtils,
    validator::{validator_authority, validator_authority_id},
};
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_message::Message;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_transaction::Transaction;
use tokio::{select, task::JoinHandle, time::Duration};
use tokio_util::{
    sync::CancellationToken,
    time::{delay_queue::Key, DelayQueue},
};
use tracing::*;

use crate::{
    db::{DbTask, SchedulerDatabase},
    errors::{TaskSchedulerError, TaskSchedulerResult},
};

struct PendingExecution {
    tx: Transaction,
    signature: Signature,
    blockhash: Hash,
    execution_started_at: i64,
    retry_attempt: u32,
}

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
    /// Retry state for tasks whose current execution is still in flight
    pending_executions: HashMap<i64, PendingExecution>,
    /// Counter used to make each transaction unique
    tx_counter: AtomicU64,
    /// Token used to cancel the task scheduler
    token: CancellationToken,
    /// Minimum interval between task executions
    min_interval: Duration,
    /// Slot interval of the validator
    slot_interval: Duration,
}

enum ProcessingOutcome {
    Success,
    Recoverable(Box<TaskSchedulerError>),
}

enum TaskExecutionResult {
    Success {
        execution_started_at: i64,
        signature: Signature,
    },
    Retry {
        delay: Duration,
    },
}

pub const TASK_EXECUTION_RETRY_LIMIT: u32 = 100;

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
            pending_executions: HashMap::new(),
            tx_counter: AtomicU64::default(),
            token,
            min_interval: config.min_interval,
            slot_interval,
        })
    }

    pub async fn start(
        mut self,
    ) -> TaskSchedulerResult<JoinHandle<TaskSchedulerResult<()>>> {
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
                || task.executions_left < 0
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

        Ok(tokio::spawn(self.run()))
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

        self.remove_task_from_queue(cancel_request.task_id);

        // Remove task from database
        self.unregister_task(cancel_request.task_id).await?;

        Ok(())
    }

    async fn execute_task(
        &mut self,
        task: &DbTask,
    ) -> TaskSchedulerResult<TaskExecutionResult> {
        if !self.pending_executions.contains_key(&task.id) {
            let execution = self.prepare_execution(task);
            self.pending_executions.insert(task.id, execution);
        }

        let retry_delay =
            Duration::from_millis(task.execution_interval_millis as u64);
        let send_result = {
            let execution = self
                .pending_executions
                .get(&task.id)
                .expect("pending execution was initialized");
            self.rpc_client.send_transaction(&execution.tx).await
        };

        match send_result {
            Ok(signature) => {
                let execution = self
                    .pending_executions
                    .remove(&task.id)
                    .expect("pending execution exists on success");
                Ok(TaskExecutionResult::Success {
                    execution_started_at: execution.execution_started_at,
                    signature,
                })
            }
            Err(err) => {
                let (signature, blockhash, retry_attempt) = {
                    let execution = self
                        .pending_executions
                        .get(&task.id)
                        .expect("pending execution exists on retry");
                    (
                        execution.signature,
                        execution.blockhash,
                        execution.retry_attempt,
                    )
                };

                if retry_attempt == TASK_EXECUTION_RETRY_LIMIT {
                    self.pending_executions.remove(&task.id);
                    return Err(Box::new(err).into());
                }

                if !self.is_blockhash_valid(&blockhash).await {
                    self.pending_executions.remove(&task.id);
                    return Err(Box::new(err).into());
                }

                warn!(
                    error = ?err,
                    %signature,
                    retry_attempt,
                    retry_limit = TASK_EXECUTION_RETRY_LIMIT,
                    "Failed to send scheduled task transaction, requeueing retry"
                );
                if let Some(execution) =
                    self.pending_executions.get_mut(&task.id)
                {
                    execution.retry_attempt += 1;
                }

                Ok(TaskExecutionResult::Retry { delay: retry_delay })
            }
        }
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

        self.pending_executions.remove(&task.id);
        self.db.insert_task(&task).await?;
        self.task_queue
            .insert(task.clone(), Duration::from_millis(0));
        debug!("Registered task {} from context", task.id);

        Ok(())
    }

    pub async fn unregister_task(
        &self,
        task_id: i64,
    ) -> TaskSchedulerResult<()> {
        self.db.remove_task(task_id).await?;
        debug!("Removed task {} from database", task_id);

        Ok(())
    }

    pub async fn run(mut self) -> TaskSchedulerResult<()> {
        loop {
            select! {
                Some(task) = self.task_queue.next() => {
                    let task = task.into_inner();
                    let task_id = task.id;
                    self.task_queue_keys.remove(&task_id);
                    match self.execute_task(&task).await {
                        Ok(TaskExecutionResult::Success {
                            execution_started_at,
                            signature,
                        }) => {
                            // TODO(Dodecahedr0x): we don't get any output directly at this point
                            // we would have to fetch the transaction via its signature to see
                            // if it succeeded or failed.
                            // However that should not happen here, but on a separate task
                            // If any instruction fails, the task is cancelled
                            debug!("Executed task {} with signature {}", task.id, signature);

                            self.db
                                .update_task_after_execution(task.id, execution_started_at)
                                .await?;

                            if task.executions_left > 1 {
                                // Reschedule the task at the original cadence, without adding retry drift.
                                let current_time = chrono::Utc::now().timestamp_millis();
                                let execution_interval_millis =
                                    task.execution_interval_millis;
                                let new_task = DbTask {
                                    executions_left: task.executions_left - 1,
                                    last_execution_millis: execution_started_at,
                                    ..task
                                };
                                let task_id = new_task.id;
                                let key = self.task_queue.insert(
                                    new_task,
                                    next_execution_delay(
                                        execution_started_at,
                                        execution_interval_millis,
                                        current_time,
                                    ),
                                );
                                self.task_queue_keys.insert(task_id, key);
                            }
                        }
                        Ok(TaskExecutionResult::Retry { delay }) => {
                            let key = self.task_queue.insert(task, delay);
                            self.task_queue_keys.insert(task_id, key);
                        }
                        Err(e) => {
                            error!("Failed to execute task {}: {}", task_id, e);

                            // If any instruction fails, the task is cancelled
                            self.pending_executions.remove(&task_id);
                            self.db.remove_task(task_id).await?;
                            self.db.insert_failed_task(task_id, format!("{:?}", e)).await?;
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
        self.pending_executions.remove(&task_id);
    }

    fn prepare_execution(&self, task: &DbTask) -> PendingExecution {
        let blockhash = self.block.load().blockhash;
        // Execute unsigned transactions
        // We prepend a noop instruction to make each transaction unique.
        let noop_instruction = InstructionUtils::noop_instruction(
            self.tx_counter.fetch_add(1, Ordering::Relaxed),
        );
        let tx = Transaction::new(
            &[validator_authority()],
            Message::new(
                &[noop_instruction]
                    .into_iter()
                    .chain(task.instructions.iter().cloned())
                    .collect::<Vec<_>>(),
                Some(&validator_authority_id()),
            ),
            blockhash,
        );
        PendingExecution {
            signature: tx.signatures[0],
            tx,
            blockhash,
            execution_started_at: chrono::Utc::now().timestamp_millis(),
            retry_attempt: 1,
        }
    }

    async fn is_blockhash_valid(&self, blockhash: &Hash) -> bool {
        match self
            .rpc_client
            .is_blockhash_valid(blockhash, CommitmentConfig::processed())
            .await
        {
            Ok(valid) => valid,
            Err(err) => {
                trace!(
                    error = ?err,
                    %blockhash,
                    "Failed to check scheduled task blockhash validity"
                );
                true
            }
        }
    }
}

fn is_valid_task_interval(interval: i64) -> bool {
    interval > 0 && interval < u32::MAX as i64
}

fn next_execution_delay(
    execution_started_at: i64,
    execution_interval_millis: i64,
    current_time: i64,
) -> Duration {
    let next_execution =
        execution_started_at.saturating_add(execution_interval_millis);
    Duration::from_millis(
        next_execution.saturating_sub(current_time).max(0) as u64
    )
}

#[cfg(test)]
mod tests {
    use magicblock_program::{
        args::ScheduleTaskRequest,
        validator::generate_validator_authority_if_needed,
    };
    use solana_pubkey::Pubkey;
    use tokio::{sync::mpsc, time::timeout};

    use super::*;

    #[tokio::test]
    async fn test_schedule_invalid_tasks() {
        magicblock_core::logger::init_for_tests();
        generate_validator_authority_if_needed();

        let (tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();

        let service = TaskSchedulerService {
            db: db.clone(),
            rpc_client: RpcClient::new("http://localhost:8899".to_string()),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            pending_executions: HashMap::new(),
            tx_counter: AtomicU64::default(),
            token: CancellationToken::new(),
            min_interval: Duration::from_millis(1000),
            slot_interval: Duration::from_millis(1000),
            scheduled_tasks: rx,
        };

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
        let service = TaskSchedulerService {
            db: db.clone(),
            rpc_client: RpcClient::new("http://localhost:8899".to_string()),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            pending_executions: HashMap::new(),
            tx_counter: AtomicU64::default(),
            token: CancellationToken::new(),
            min_interval: Duration::from_millis(1000),
            slot_interval: Duration::from_millis(1000),
            scheduled_tasks: rx,
        };

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

    #[test]
    fn test_next_execution_delay_avoids_retry_drift() {
        let delay = next_execution_delay(1_000, 100, 1_030);
        assert_eq!(delay, Duration::from_millis(70));

        let delay = next_execution_delay(1_000, 100, 1_150);
        assert_eq!(delay, Duration::ZERO);
    }

    #[tokio::test]
    async fn test_execute_task_requeues_retry_without_sleeping() {
        magicblock_core::logger::init_for_tests();
        generate_validator_authority_if_needed();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        let mut service = TaskSchedulerService {
            db,
            rpc_client: RpcClient::new("http://127.0.0.1:65535".to_string()),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            pending_executions: HashMap::new(),
            tx_counter: AtomicU64::default(),
            token: CancellationToken::new(),
            min_interval: Duration::from_millis(1000),
            slot_interval: Duration::from_millis(1000),
            scheduled_tasks: rx,
        };

        let task = DbTask {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: 30_000,
            executions_left: 1,
            last_execution_millis: 0,
            instructions: vec![],
        };

        let result =
            timeout(Duration::from_secs(1), service.execute_task(&task))
                .await
                .expect("retry path should not sleep inline")
                .unwrap();

        match result {
            TaskExecutionResult::Retry { delay } => {
                assert_eq!(delay, Duration::from_secs(30));
                assert_eq!(
                    service
                        .pending_executions
                        .get(&task.id)
                        .as_ref()
                        .unwrap()
                        .retry_attempt,
                    2
                );
            }
            TaskExecutionResult::Success { .. } => {
                panic!("task should have been requeued for retry");
            }
        }
    }
}
