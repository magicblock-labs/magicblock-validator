use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use futures_util::{future::poll_fn, FutureExt, StreamExt};
use magicblock_config::config::TaskSchedulerConfig;
use magicblock_core::link::transactions::ScheduledTasksRx;
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    args::{CancelTaskRequest, ScheduleTaskRequest, TaskRequest},
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
    time::{interval, Duration, MissedTickBehavior},
};
use tokio_util::{
    sync::CancellationToken,
    time::{delay_queue::Key, DelayQueue},
};
use tracing::*;

use crate::{
    db::{
        CrankFailedMove, CrankRetryCheck, CrankSuccessRemoval,
        CrankSuccessUpdate, DbTask, SchedulerDatabase,
    },
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
    /// Latest in-memory instance version for queued or in-flight tasks
    task_versions: HashMap<i64, i64>,
    /// Number of consecutive failed execution attempts for each task
    task_execution_retries: HashMap<i64, u32>,
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
    /// Creates a new `TaskSchedulerService` with the given configuration.
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
            task_versions: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: Arc::new(AtomicU64::default()),
            token,
            min_interval: config.min_interval,
            failed_task_retention: config.failed_task_retention,
            failed_task_cleanup_interval: config.failed_task_cleanup_interval,
            slot_interval,
        })
    }

    /// Starts the `TaskSchedulerService` and returns a handle to the task.
    pub async fn start(
        mut self,
    ) -> TaskSchedulerResult<JoinHandle<TaskSchedulerResult<()>>> {
        self.load_persisted_tasks().await?;
        Ok(tokio::spawn(self.run()))
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
            self.task_versions.insert(task_id, task.updated_at);
            let key = self.task_queue.insert(task, timeout);
            self.task_queue_keys.insert(task_id, key);
        }

        Ok(())
    }

    /// Main loop of the task scheduler.
    async fn run(mut self) -> TaskSchedulerResult<()> {
        let mut failed_task_cleanup = interval(
            self.failed_task_cleanup_interval
                .max(Duration::from_millis(1)),
        );
        failed_task_cleanup.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let (crank_tx, mut crank_rx) = mpsc::unbounded_channel();

        loop {
            select! {
                Some(expired) = self.task_queue.next() => {
                    // A task expired, batch all expired tasks
                    let first = expired.into_inner();
                    self.task_queue_keys.remove(&first.id);
                    let mut batch = vec![first];
                    while let Some(expired) = poll_fn(|cx| self.task_queue.poll_expired(cx))
                        .now_or_never()
                        .flatten()
                    {
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
                            Self::send_crank_batch(rpc_client, &block, tx_counter, &batch).await;
                        let _ = crank_tx.send((batch, result));
                    });
                }
                Some((batch, result)) = crank_rx.recv() => {
                    // The batch has been sent, updates queue and db
                    self.on_crank_batch_completed(batch, result).await?;
                }
                Some(task) = self.scheduled_tasks.recv() => {
                    // Received a new request from the transaction executor
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
        drop(crank_tx);
        while let Some((batch, result)) = crank_rx.recv().await {
            self.on_crank_batch_completed(batch, result).await?;
        }

        Ok(())
    }

    /// Processes [TaskRequest] provided by the transaction executor.
    async fn process_request(
        &mut self,
        request: TaskRequest,
    ) -> TaskSchedulerResult<ProcessingOutcome> {
        let task_id = request.id();
        match request {
            TaskRequest::Schedule(schedule_request) => {
                if let Err(e) =
                    self.process_schedule_request(schedule_request).await
                {
                    self.db
                        .insert_failed_scheduling(task_id, format!("{:?}", e))
                        .await?;
                    error!(
                        "Failed to process schedule request {}: {}",
                        task_id, e
                    );

                    return Ok(ProcessingOutcome::Recoverable(Box::new(e)));
                }
            }
            TaskRequest::Cancel(cancel_request) => {
                if let Err(e) =
                    self.process_cancel_request(&cancel_request).await
                {
                    self.db
                        .insert_failed_scheduling(task_id, format!("{:?}", e))
                        .await?;
                    error!(
                        "Failed to process cancel request for task {}: {}",
                        task_id, e
                    );

                    return Ok(ProcessingOutcome::Recoverable(Box::new(e)));
                }
            }
        };

        Ok(ProcessingOutcome::Success)
    }

    /// Processes [ScheduleTaskRequest] provided by the transaction executor.
    async fn process_schedule_request(
        &mut self,
        mut task: ScheduleTaskRequest,
    ) -> TaskSchedulerResult<()> {
        if !is_valid_task_interval(task.execution_interval_millis) {
            // If the interval is too large or zero, we don't schedule the task
            return Ok(());
        }

        task.execution_interval_millis = task
            .execution_interval_millis
            .clamp(self.min_interval.as_millis() as i64, u32::MAX as i64);

        let mut task = DbTask::from(task);
        let task_id = task.id;

        // Check if the task already exists in the database
        if let Some(db_task) = self.db.get_task(task_id).await? {
            if db_task.authority != task.authority {
                return Err(TaskSchedulerError::UnauthorizedReplacing(
                    task_id,
                    db_task.authority.to_string(),
                    task.authority.to_string(),
                ));
            }
        }

        task.updated_at = self.db.insert_task(&task).await?;
        self.remove_task_from_queue(task_id);
        self.task_execution_retries.remove(&task_id);
        self.task_versions.insert(task_id, task.updated_at);
        let key = self.task_queue.insert(task, Duration::from_millis(0));
        self.task_queue_keys.insert(task_id, key);
        debug!("Registered task {} from context", task_id);

        Ok(())
    }

    /// Processes [CancelTaskRequest] provided by the transaction executor.
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
        self.remove_task_runtime_state(task.id);
        self.task_execution_retries.remove(&task.id);
        self.db.remove_task(task.id).await?;
        debug!("Removed task {} from database", task.id);

        Ok(())
    }

    /// Sends a batch of crank transactions to the RPC client.
    async fn send_crank_batch(
        rpc_client: Arc<RpcClient>,
        block: &LatestBlock,
        tx_counter: Arc<AtomicU64>,
        tasks: &[DbTask],
    ) -> TaskSchedulerResult<Vec<(DbTask, TaskSchedulerResult<Signature>)>>
    {
        let mut join_set: JoinSet<(DbTask, TaskSchedulerResult<Signature>)> =
            JoinSet::new();
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
                let res = rpc_client
                    .send_transaction(&tx)
                    .await
                    .map_err(Box::new)
                    .map_err(TaskSchedulerError::from);
                (task, res)
            });
        }
        Ok(join_set.join_all().await)
    }

    /// Called when a crank batch is completed.
    async fn on_crank_batch_completed(
        &mut self,
        batch: Vec<DbTask>,
        result: TaskSchedulerResult<
            Vec<(DbTask, TaskSchedulerResult<Signature>)>,
        >,
    ) -> TaskSchedulerResult<()> {
        let now_millis = chrono::Utc::now().timestamp_millis();
        let mut success_updates: Vec<CrankSuccessUpdate> = Vec::new();
        let mut success_removals: Vec<CrankSuccessRemoval> = Vec::new();
        let mut failed_records: Vec<CrankFailedMove> = Vec::new();
        let mut retry_checks: Vec<CrankRetryCheck> = Vec::new();

        // Decide what must happen to cranks
        match result {
            Ok(ref result) => {
                // Batch completed, update individual crank based on tx status
                for (task, res) in result {
                    if let Err(e) = res {
                        self.prepare_crank_failure_outcome(
                            task,
                            e,
                            &mut failed_records,
                            &mut retry_checks,
                        )?;
                    } else {
                        self.prepare_crank_success_outcome(
                            task,
                            now_millis,
                            &mut success_updates,
                            &mut success_removals,
                        )?;
                    }
                }
            }
            Err(ref e) => {
                // Whole batch failed, fail all cranks
                for task in &batch {
                    self.prepare_crank_failure_outcome(
                        task,
                        e,
                        &mut failed_records,
                        &mut retry_checks,
                    )?;
                }
            }
        }

        // Apply db updates for the whole batch
        let completion = self
            .db
            .apply_crank_batch_completion(
                &success_updates,
                &success_removals,
                &failed_records,
                &retry_checks,
            )
            .await?;

        // Update queue, retries and versions based on decisions
        match result {
            Ok(result) => {
                for (task, res) in &result {
                    if let Err(e) = res {
                        if completion.failed_moves.get(&task.id).is_some_and(
                            |m| m.expected_updated_at == task.updated_at,
                        ) || completion
                            .retry_checks
                            .get(&task.id)
                            .is_some_and(|c| {
                                c.current_updated_at == task.updated_at
                            })
                        {
                            self.apply_crank_failure_outcome(task, e)?;
                        }
                    } else if let Some(update) =
                        completion.success_updates.get(&task.id)
                    {
                        if update.expected_updated_at == task.updated_at {
                            self.apply_crank_success_outcome(
                                task,
                                now_millis,
                                Some(update.new_updated_at),
                            )?;
                        }
                    } else if completion
                        .success_removals
                        .get(&task.id)
                        .is_some_and(|r| {
                            r.expected_updated_at == task.updated_at
                        })
                    {
                        self.apply_crank_success_outcome(
                            task, now_millis, None,
                        )?;
                    }
                }
            }
            Err(ref e) => {
                for task in &batch {
                    if completion.failed_moves.get(&task.id).is_some_and(|m| {
                        m.expected_updated_at == task.updated_at
                    }) || completion.retry_checks.get(&task.id).is_some_and(
                        |c| c.current_updated_at == task.updated_at,
                    ) {
                        self.apply_crank_failure_outcome(task, e)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Either:
    /// - reschedules crank with remaining iterations
    /// - remove exhausted cranks
    fn prepare_crank_success_outcome(
        &self,
        task: &DbTask,
        now_millis: i64,
        success_updates: &mut Vec<CrankSuccessUpdate>,
        success_removals: &mut Vec<CrankSuccessRemoval>,
    ) -> TaskSchedulerResult<()> {
        let executed_at = next_execution_millis(
            task.last_execution_millis,
            task.execution_interval_millis,
            now_millis,
        );

        if task.executions_left > 1 {
            success_updates.push(CrankSuccessUpdate {
                task_id: task.id,
                last_execution_millis: executed_at,
                expected_updated_at: task.updated_at,
            });
        } else {
            success_removals.push(CrankSuccessRemoval {
                task_id: task.id,
                expected_updated_at: task.updated_at,
            });
        }

        Ok(())
    }

    /// Either:
    /// - re-queues for retry
    /// - queues a permanent failure for SQLite along with clearing retry counters and stale queue keys.
    fn prepare_crank_failure_outcome(
        &self,
        task: &DbTask,
        error: &TaskSchedulerError,
        failed_records: &mut Vec<CrankFailedMove>,
        retry_checks: &mut Vec<CrankRetryCheck>,
    ) -> TaskSchedulerResult<()> {
        debug!("Failed to execute task {}: {}", task.id, error);

        if !is_retryable_task_execution_error(error) {
            // Unretryable crank are moved to failed cranks
            failed_records.push(CrankFailedMove {
                task_id: task.id,
                expected_updated_at: task.updated_at,
                error: error.to_string(),
            });
            return Ok(());
        }

        let retries = *self.task_execution_retries.get(&task.id).unwrap_or(&0);
        if retries >= MAX_TASK_EXECUTION_RETRIES {
            // Crank exhausted retries, fail it
            failed_records.push(CrankFailedMove {
                task_id: task.id,
                expected_updated_at: task.updated_at,
                error: error.to_string(),
            });
            return Ok(());
        }

        // Schedule for retry
        retry_checks.push(CrankRetryCheck {
            task_id: task.id,
            expected_updated_at: task.updated_at,
        });

        Ok(())
    }

    /// Updates the delay queue for the next firing after SQLite confirms the same task instance.
    fn apply_crank_success_outcome(
        &mut self,
        task: &DbTask,
        now_millis: i64,
        updated_at: Option<i64>,
    ) -> TaskSchedulerResult<()> {
        if !self.task_version_matches(task) {
            debug!(
                task_id = task.id,
                expected_updated_at = task.updated_at,
                "Skipping stale successful crank completion"
            );
            return Ok(());
        }

        let executed_at = next_execution_millis(
            task.last_execution_millis,
            task.execution_interval_millis,
            now_millis,
        );

        if let Some(updated_at) = updated_at {
            let next_execution = executed_at + task.execution_interval_millis;
            let delay = delay_until_millis(next_execution, now_millis);
            let new_task = DbTask {
                executions_left: task.executions_left - 1,
                last_execution_millis: executed_at,
                updated_at,
                ..task.clone()
            };
            let key = self.task_queue.insert(new_task, delay);
            self.task_queue_keys.insert(task.id, key);
            self.task_versions.insert(task.id, updated_at);
        } else {
            self.remove_task_runtime_state(task.id);
        }

        self.task_execution_retries.remove(&task.id);
        Ok(())
    }

    /// Either:
    /// - re-queues for retry
    /// - records a permanent failure in runtime state after SQLite confirms the same task instance.
    fn apply_crank_failure_outcome(
        &mut self,
        task: &DbTask,
        error: &TaskSchedulerError,
    ) -> TaskSchedulerResult<()> {
        if !self.task_version_matches(task) {
            debug!(
                task_id = task.id,
                expected_updated_at = task.updated_at,
                "Skipping stale failed crank completion"
            );
            return Ok(());
        }

        if !is_retryable_task_execution_error(error) {
            self.task_execution_retries.remove(&task.id);
            self.remove_task_runtime_state(task.id);
            return Ok(());
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
            self.task_execution_retries.remove(&task.id);
            self.remove_task_runtime_state(task.id);
            return Ok(());
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
        self.task_versions.insert(task.id, task.updated_at);

        Ok(())
    }

    fn task_version_matches(&self, task: &DbTask) -> bool {
        self.task_versions.get(&task.id) == Some(&task.updated_at)
    }

    /// Removes a task from the queue.
    fn remove_task_from_queue(&mut self, task_id: i64) {
        if let Some(key) = self.task_queue_keys.remove(&task_id) {
            self.task_queue.remove(&key);
        }
    }

    fn remove_task_runtime_state(&mut self, task_id: i64) {
        self.remove_task_from_queue(task_id);
        self.task_versions.remove(&task_id);
    }

    /// Calculates the retry delay for the next task execution.
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

fn is_valid_task_interval(interval: i64) -> bool {
    interval > 0 && interval < u32::MAX as i64
}

fn next_execution_millis(
    last_execution_millis: i64,
    execution_interval_millis: i64,
    now: i64,
) -> i64 {
    if last_execution_millis == 0 {
        now
    } else {
        last_execution_millis + execution_interval_millis
    }
}

fn delay_until_millis(execution_millis: i64, now: i64) -> Duration {
    Duration::from_millis(execution_millis.saturating_sub(now).max(0) as u64)
}

fn is_retryable_task_execution_error(error: &TaskSchedulerError) -> bool {
    // `send_crank_batch` maps Solana send and verification failures to Rpc.
    matches!(error, TaskSchedulerError::Rpc(_))
}

#[cfg(test)]
mod tests {
    use magicblock_core::coordination_mode::switch_to_primary_mode;
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
            rpc_client: Arc::new(RpcClient::new(
                "http://localhost:8899".to_string(),
            )),
            block: LatestBlock::default(),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            task_versions: HashMap::new(),
            task_execution_retries: HashMap::new(),
            tx_counter: Arc::new(AtomicU64::default()),
            token: CancellationToken::new(),
            min_interval: Duration::from_millis(1000),
            failed_task_retention: Duration::from_secs(60),
            failed_task_cleanup_interval: Duration::from_secs(60),
            slot_interval: Duration::from_millis(1000),
            scheduled_tasks,
        }
    }

    #[serial]
    #[test]
    fn test_first_execution_anchors_cadence_at_now() {
        assert_eq!(next_execution_millis(0, 50, 1_000), 1_000);
    }

    #[serial]
    #[test]
    fn test_recurring_execution_preserves_fixed_rate_cadence() {
        let executed_at = next_execution_millis(1_000, 50, 1_090);
        assert_eq!(executed_at, 1_050);

        let delay = delay_until_millis(executed_at + 50, 1_090);
        assert_eq!(delay, Duration::from_millis(10));
    }

    #[serial]
    #[test]
    fn test_overdue_execution_is_rescheduled_immediately() {
        assert_eq!(delay_until_millis(1_100, 1_150), Duration::from_millis(0));
    }

    #[serial]
    #[tokio::test]
    async fn test_schedule_invalid_tasks() {
        magicblock_core::logger::init_for_tests();
        switch_to_primary_mode();
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
        switch_to_primary_mode();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        // Invalid task interval
        db.insert_task(&DbTask {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: u32::MAX as i64,
            executions_left: 1,
            last_execution_millis: chrono::Utc::now().timestamp_millis(),
            updated_at: 0,
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
            updated_at: 0,
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
        switch_to_primary_mode();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        db.insert_task(&DbTask {
            id: 1,
            authority: Pubkey::new_unique(),
            execution_interval_millis: 50,
            executions_left: 0,
            last_execution_millis: chrono::Utc::now().timestamp_millis(),
            updated_at: 0,
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
            updated_at: 0,
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
    async fn test_stale_crank_completion_does_not_mutate_replaced_task() {
        magicblock_core::logger::init_for_tests();
        switch_to_primary_mode();

        let (_tx, rx) = mpsc::unbounded_channel();
        let db = SchedulerDatabase::new(":memory:").unwrap();
        let authority = Pubkey::new_unique();
        let mut old_task = DbTask {
            id: 1,
            authority,
            execution_interval_millis: 50,
            executions_left: 2,
            last_execution_millis: 0,
            updated_at: 0,
            instructions: vec![],
        };
        old_task.updated_at = db.insert_task(&old_task).await.unwrap();

        let mut service = test_service(db.clone(), rx);

        let mut replacement = DbTask {
            executions_left: 5,
            ..old_task.clone()
        };
        replacement.updated_at = db.insert_task(&replacement).await.unwrap();
        let key = service
            .task_queue
            .insert(replacement.clone(), Duration::from_secs(10));
        service.task_queue_keys.insert(replacement.id, key);
        service
            .task_versions
            .insert(replacement.id, replacement.updated_at);

        service
            .on_crank_batch_completed(
                vec![old_task.clone()],
                Ok(vec![(old_task, Ok(Signature::new_unique()))]),
            )
            .await
            .unwrap();

        let persisted = db.get_task(replacement.id).await.unwrap().unwrap();
        assert_eq!(persisted.executions_left, replacement.executions_left);
        assert_eq!(persisted.updated_at, replacement.updated_at);

        let key = service.task_queue_keys.remove(&replacement.id).unwrap();
        let queued = service.task_queue.remove(&key).into_inner();
        assert_eq!(queued.updated_at, replacement.updated_at);
        assert_eq!(queued.executions_left, replacement.executions_left);
    }

    #[serial]
    #[tokio::test]
    async fn test_failed_records_are_cleaned_up_periodically() {
        magicblock_core::logger::init_for_tests();
        switch_to_primary_mode();

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
}
