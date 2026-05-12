use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use futures_util::{task::noop_waker, StreamExt};
use magicblock_config::config::TaskSchedulerConfig;
use magicblock_core::link::transactions::ScheduledTasksRx;
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    args::{CancelTaskRequest, TaskRequest},
    instruction_utils::InstructionUtils,
    validator::{validator_authority, validator_authority_id},
};
use solana_message::Message;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signature::Signature;
use solana_transaction::Transaction;
use tokio::{
    select,
    sync::mpsc,
    task::{JoinHandle, JoinSet},
    time::Duration,
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
    rpc_client: Arc<RpcClient>,
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
    /// Token used to cancel the task scheduler
    token: CancellationToken,
    /// Minimum interval between task executions
    min_interval: Duration,
    /// Slot interval of the validator
    slot_interval: Duration,
    /// Shared counter for noop instructions (unique crank transactions).
    tx_counter: Arc<AtomicU64>,
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
            rpc_client: Arc::new(RpcClient::new(rpc_url)),
            scheduled_tasks,
            block,
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: Arc::new(AtomicU64::default()),
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

    async fn apply_successful_execution(
        &mut self,
        task: &DbTask,
    ) -> TaskSchedulerResult<()> {
        let Some(current) = self.db.get_task(task.id).await? else {
            self.task_execution_retries.remove(&task.id);
            return Ok(());
        };

        if task.executions_left <= 1 {
            self.db.remove_task(task.id).await?;
        } else {
            let current_time = chrono::Utc::now().timestamp_millis();
            self.db
                .update_task_after_execution(task.id, current_time)
                .await?;
        }

        if current.executions_left > 1 {
            let new_task = DbTask {
                executions_left: current.executions_left - 1,
                ..current
            };
            let key = self.task_queue.insert(
                new_task,
                Duration::from_millis(current.execution_interval_millis as u64),
            );
            self.task_queue_keys.insert(task.id, key);
        }

        self.task_execution_retries.remove(&task.id);
        Ok(())
    }

    async fn on_crank_batch_completed(
        &mut self,
        batch: Vec<DbTask>,
        result: TaskSchedulerResult<Vec<TaskSchedulerResult<Signature>>>,
    ) -> TaskSchedulerResult<()> {
        match result {
            Ok(sigs) => {
                debug!(
                    "Executed crank batch ({} tasks) with signatures {:?}",
                    batch.len(),
                    sigs
                );
                // Note: batch and sigs have the same length by design
                for (task, res) in batch.iter().zip(sigs) {
                    // Handle tasks individually if they failed
                    if let Err(e) = res {
                        self.handle_failed_task_execution(task, &e).await?;
                    } else {
                        self.apply_successful_execution(task).await?;
                    }
                }
            }
            Err(ref e) => {
                // The whole batch failed
                for task in &batch {
                    self.handle_failed_task_execution(task, e).await?;
                }
            }
        }
        Ok(())
    }

    async fn register_task(
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

    async fn unregister_task(
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
        let (crank_tx, mut crank_rx) = mpsc::unbounded_channel();

        loop {
            select! {
                Some(expired) = self.task_queue.next() => {
                    let first = expired.into_inner();
                    self.task_queue_keys.remove(&first.id);
                    let mut batch = vec![first];
                    let waker = noop_waker();
                    let mut cx = Context::from_waker(&waker);
                    while let Poll::Ready(Some(expired)) = self.task_queue.poll_expired(&mut cx) {
                        let task = expired.into_inner();
                        self.task_queue_keys.remove(&task.id);
                        batch.push(task);
                    }

                    let rpc_client = self.rpc_client.clone();
                    let block = self.block.clone();
                    let tx_counter = self.tx_counter.clone();
                    let crank_tx = crank_tx.clone();
                    tokio::spawn(async move {
                        let result =
                            send_crank_batch(rpc_client, &block, tx_counter, &batch).await;
                        let _ = crank_tx.send((batch, result));
                    });
                }
                Some((batch, result)) = crank_rx.recv() => {
                    self.on_crank_batch_completed(batch, result).await?;
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
    }

    async fn handle_failed_task_execution(
        &mut self,
        task: &DbTask,
        error: &TaskSchedulerError,
    ) -> TaskSchedulerResult<()> {
        debug!("Failed to execute task {}: {}", task.id, error);

        if !is_retryable_task_execution_error(error) {
            return self.record_failed_task(task, error.to_string()).await;
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
            return self.record_failed_task(task, error.to_string()).await;
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
        error: String,
    ) -> TaskSchedulerResult<()> {
        self.db.move_task_to_failed(task.id, error).await?;
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
}

async fn send_crank_batch(
    rpc_client: Arc<RpcClient>,
    block: &LatestBlock,
    tx_counter: Arc<AtomicU64>,
    tasks: &[DbTask],
) -> TaskSchedulerResult<Vec<TaskSchedulerResult<Signature>>> {
    let mut join_set: JoinSet<TaskSchedulerResult<Signature>> = JoinSet::new();
    let blockhash = block.load().blockhash;
    for task in tasks {
        let rpc_client = rpc_client.clone();
        let tx_counter = tx_counter.clone();
        let task = task.clone();
        join_set.spawn(async move {
            let ixs = vec![
                InstructionUtils::noop_instruction(
                    tx_counter.fetch_add(1, Ordering::Relaxed),
                ),
                InstructionUtils::execute_task_instruction(
                    task.instructions.clone(),
                ),
            ];
            let tx = Transaction::new(
                &[validator_authority()],
                Message::new(&ixs, Some(&validator_authority_id())),
                blockhash,
            );
            Ok(rpc_client.send_transaction(&tx).await.map_err(Box::new)?)
        });
    }
    let results = join_set.join_all().await;
    let sigs = results
        .into_iter()
        .collect::<Vec<TaskSchedulerResult<Signature>>>();
    Ok(sigs)
}

fn is_valid_task_interval(interval: i64) -> bool {
    interval > 0 && interval < u32::MAX as i64
}

fn is_retryable_task_execution_error(error: &TaskSchedulerError) -> bool {
    // `send_crank_batch` maps Solana send and verification failures to Rpc.
    matches!(error, TaskSchedulerError::Rpc(_))
}

#[cfg(test)]
mod tests {
    use std::sync;

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
            rpc_client: Arc::new(RpcClient::new(
                "http://localhost:8899".to_string(),
            )),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: sync::Arc::new(AtomicU64::default()),
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
            rpc_client: Arc::new(RpcClient::new(
                "http://localhost:8899".to_string(),
            )),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: sync::Arc::new(AtomicU64::default()),
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

        let service = TaskSchedulerService {
            db: db.clone(),
            rpc_client: Arc::new(RpcClient::new(
                "http://localhost:8899".to_string(),
            )),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: Arc::new(AtomicU64::default()),
            token: CancellationToken::new(),
            min_interval: Duration::from_millis(10),
            slot_interval: Duration::from_millis(1000),
            scheduled_tasks: rx,
        };

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
}
