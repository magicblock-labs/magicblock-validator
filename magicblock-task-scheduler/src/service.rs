use std::{
    collections::HashMap,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use futures_util::StreamExt;
use log::*;
use magicblock_config::config::TaskSchedulerConfig;
use magicblock_core::link::transactions::{
    ScheduledTasksRx, TransactionSchedulerHandle,
};
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    args::{CancelTaskRequest, TaskRequest},
    task_scheduler_frequency::set_min_task_scheduler_interval,
    validator::{validator_authority, validator_authority_id},
};
use solana_sdk::{
    instruction::Instruction, message::Message, pubkey::Pubkey,
    signature::Signature, transaction::Transaction,
};
use tokio::{select, task::JoinHandle, time::Duration};
use tokio_util::{
    sync::CancellationToken,
    time::{delay_queue::Key, DelayQueue},
};

use crate::{
    db::{DbTask, SchedulerDatabase},
    errors::{TaskSchedulerError, TaskSchedulerResult},
};

const NOOP_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV");

pub struct TaskSchedulerService {
    /// Database for persisting tasks
    db: SchedulerDatabase,
    /// Used to send transactions for execution
    tx_scheduler: TransactionSchedulerHandle,
    /// Used to receive scheduled tasks from the transaction executor
    scheduled_tasks: ScheduledTasksRx,
    /// Provides latest blockhash for signing transactions
    block: LatestBlock,
    /// Queue of tasks to execute
    task_queue: DelayQueue<DbTask>,
    /// Map of task IDs to their corresponding keys in the task queue
    task_queue_keys: HashMap<i64, Key>,
    /// Counter used to make each transaction unique
    tx_counter: AtomicU64,
    /// Token used to cancel the task scheduler
    token: CancellationToken,
}

enum ProcessingOutcome {
    Success,
    Recoverable(TaskSchedulerError),
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
        tx_scheduler: TransactionSchedulerHandle,
        scheduled_tasks: ScheduledTasksRx,
        block: LatestBlock,
        token: CancellationToken,
    ) -> Result<Self, TaskSchedulerError> {
        set_min_task_scheduler_interval(config.min_interval_millis);
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
            tx_scheduler,
            scheduled_tasks,
            block,
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            tx_counter: AtomicU64::default(),
            token,
        })
    }

    pub async fn start(
        mut self,
    ) -> TaskSchedulerResult<JoinHandle<TaskSchedulerResult<()>>> {
        let tasks = self.db.get_tasks().await?;
        let now = chrono::Utc::now().timestamp_millis();
        debug!(
            "Task scheduler starting at {} with {} tasks",
            now,
            tasks.len()
        );
        for task in tasks {
            let next_execution =
                task.last_execution_millis + task.execution_interval_millis;
            let timeout = Duration::from_millis(
                next_execution.saturating_sub(now) as u64,
            );
            let task_id = task.id;
            let key = self.task_queue.insert(task, timeout);
            self.task_queue_keys.insert(task_id, key);
        }

        Ok(tokio::spawn(self.run()))
    }

    async fn process_request(
        &mut self,
        request: &TaskRequest,
    ) -> TaskSchedulerResult<ProcessingOutcome> {
        match request {
            TaskRequest::Schedule(schedule_request) => {
                if let Err(e) = self.register_task(schedule_request).await {
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

                    return Ok(ProcessingOutcome::Recoverable(e));
                }
            }
            TaskRequest::Cancel(cancel_request) => {
                if let Err(e) =
                    self.process_cancel_request(cancel_request).await
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

                    return Ok(ProcessingOutcome::Recoverable(e));
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

    async fn execute_task(&mut self, task: &DbTask) -> TaskSchedulerResult<()> {
        let sig = self.process_transaction(task.instructions.clone()).await?;

        // TODO(Dodecahedr0x): we don't get any output directly at this point
        // we would have to fetch the transaction via its signature to see
        // if it succeeded or failed.
        // However that should not happen here, but on a separate task
        // If any instruction fails, the task is cancelled
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

        let current_time = chrono::Utc::now().timestamp_millis();
        self.db
            .update_task_after_execution(task.id, current_time)
            .await?;

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
                    let task = task.get_ref();
                    self.task_queue_keys.remove(&task.id);
                    if let Err(e) = self.execute_task(task).await {
                        error!("Failed to execute task {}: {}", task.id, e);

                        // If any instruction fails, the task is cancelled
                        self.db.remove_task(task.id).await?;
                        self.db.insert_failed_task(task.id, format!("{:?}", e)).await?;
                    }
                }
                Some(task) = self.scheduled_tasks.recv() => {
                    match self.process_request(&task).await {
                        Ok(ProcessingOutcome::Success) => {}
                        Ok(ProcessingOutcome::Recoverable(e)) => {
                            warn!("Failed to process request ID={}: {e:?}", task.id());
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

        Ok(())
    }

    fn remove_task_from_queue(&mut self, task_id: i64) {
        if let Some(key) = self.task_queue_keys.remove(&task_id) {
            self.task_queue.remove(&key);
        }
    }

    async fn process_transaction(
        &self,
        instructions: Vec<Instruction>,
    ) -> TaskSchedulerResult<Signature> {
        let blockhash = self.block.load().blockhash;
        // Execute unsigned transactions
        // We prepend a noop instruction to make each transaction unique.
        let noop_instruction = Instruction::new_with_bytes(
            NOOP_PROGRAM_ID,
            &self
                .tx_counter
                .fetch_add(1, Ordering::Relaxed)
                .to_le_bytes(),
            vec![],
        );
        let tx = Transaction::new(
            &[validator_authority()],
            Message::new(
                &[noop_instruction]
                    .into_iter()
                    .chain(instructions)
                    .collect::<Vec<_>>(),
                Some(&validator_authority_id()),
            ),
            blockhash,
        );

        let sig = tx.signatures[0];
        self.tx_scheduler.execute(tx).await?;
        Ok(sig)
    }
}
