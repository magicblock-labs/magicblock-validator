use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use futures_util::StreamExt;
use log::*;
use magicblock_config::TaskSchedulerConfig;
use magicblock_core::{
    link::transactions::TransactionSchedulerHandle, traits::AccountsBank,
};
use magicblock_ledger::LatestBlock;
use magicblock_program::{
    instruction_utils::InstructionUtils,
    validator::{validator_authority, validator_authority_id},
    CancelTaskRequest, CrankTask, ScheduleTaskRequest, TaskContext,
    TaskRequest, TASK_CONTEXT_PUBKEY,
};
use solana_sdk::{
    account::ReadableAccount, instruction::Instruction, message::Message,
    pubkey::Pubkey, signature::Signature, transaction::Transaction,
};
use tokio::{select, time::Duration};
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

pub struct TaskSchedulerService<T: AccountsBank> {
    /// Database for persisting tasks
    db: SchedulerDatabase,
    /// Bank for executing tasks
    bank: Arc<T>,
    /// Used to send transactions for execution
    tx_scheduler: TransactionSchedulerHandle,
    /// Provides latest blockhash for signing transactions
    block: LatestBlock,
    /// Interval at which the task scheduler will check for requests in the context
    tick_interval: Duration,
    /// Queue of tasks to execute
    task_queue: DelayQueue<DbTask>,
    /// Map of task IDs to their corresponding keys in the task queue
    task_queue_keys: HashMap<u64, Key>,
    /// Counter used to make each transaction unique
    tx_counter: AtomicU64,
}

unsafe impl<T: AccountsBank> Send for TaskSchedulerService<T> {}
unsafe impl<T: AccountsBank> Sync for TaskSchedulerService<T> {}
impl<T: AccountsBank> TaskSchedulerService<T> {
    pub fn start(
        path: &Path,
        config: &TaskSchedulerConfig,
        bank: Arc<T>,
        tx_scheduler: TransactionSchedulerHandle,
        block: LatestBlock,
        token: CancellationToken,
    ) -> Result<
        tokio::task::JoinHandle<TaskSchedulerResult<()>>,
        TaskSchedulerError,
    > {
        debug!("Initializing task scheduler service");
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
        let tasks = db.get_tasks()?;
        let mut service = Self {
            db,
            bank,
            tx_scheduler,
            block,
            tick_interval: Duration::from_millis(config.millis_per_tick),
            task_queue: DelayQueue::new(),
            task_queue_keys: HashMap::new(),
            tx_counter: AtomicU64::default(),
        };
        let now = chrono::Utc::now().timestamp_millis() as u64;
        debug!("Task scheduler started at {}", now);
        for task in tasks {
            let next_execution =
                task.last_execution_millis + task.execution_interval_millis;
            let timeout =
                Duration::from_millis(next_execution.saturating_sub(now));
            let task_id = task.id;
            let key = service.task_queue.insert(task, timeout);
            service.task_queue_keys.insert(task_id, key);
        }

        Ok(tokio::spawn(service.run(token)))
    }

    fn process_context_requests(
        &mut self,
        requests: &Vec<TaskRequest>,
    ) -> TaskSchedulerResult<Vec<TaskSchedulerError>> {
        let mut errors = Vec::with_capacity(requests.len());
        for request in requests {
            match request {
                TaskRequest::Schedule(schedule_request) => {
                    if let Err(e) =
                        self.process_schedule_request(schedule_request)
                    {
                        self.db.insert_failed_scheduling(
                            schedule_request.id,
                            format!("{:?}", e),
                        )?;
                        error!(
                            "Failed to process schedule request {}: {}",
                            schedule_request.id, e
                        );
                        errors.push(e);
                    }
                }
                TaskRequest::Cancel(cancel_request) => {
                    if let Err(e) = self.process_cancel_request(cancel_request)
                    {
                        self.db.insert_failed_scheduling(
                            cancel_request.task_id,
                            format!("{:?}", e),
                        )?;
                        error!(
                            "Failed to process cancel request for task {}: {}",
                            cancel_request.task_id, e
                        );
                        errors.push(e);
                    }
                }
            };
        }

        Ok(errors)
    }

    fn process_schedule_request(
        &mut self,
        schedule_request: &ScheduleTaskRequest,
    ) -> TaskSchedulerResult<()> {
        // Convert request to task and register in database
        let task = CrankTask::from(schedule_request);
        self.register_task(&task)?;

        Ok(())
    }

    fn process_cancel_request(
        &mut self,
        cancel_request: &CancelTaskRequest,
    ) -> TaskSchedulerResult<()> {
        let Some(task) = self.db.get_task(cancel_request.task_id)? else {
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
        self.unregister_task(cancel_request.task_id)?;

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
                Duration::from_millis(task.execution_interval_millis),
            );
            self.task_queue_keys.insert(task.id, key);
        }

        let current_time = chrono::Utc::now().timestamp_millis();
        self.db.update_task_after_execution(task.id, current_time)?;

        Ok(())
    }

    pub fn register_task(
        &mut self,
        task: &CrankTask,
    ) -> TaskSchedulerResult<()> {
        let db_task = DbTask {
            id: task.id,
            instructions: task.instructions.clone(),
            authority: task.authority,
            execution_interval_millis: task.execution_interval_millis,
            executions_left: task.iterations,
            last_execution_millis: 0,
        };

        // Check if the task already exists in the database
        if let Some(db_task) = self.db.get_task(task.id)? {
            if db_task.authority != task.authority {
                return Err(TaskSchedulerError::UnauthorizedReplacing(
                    task.id,
                    db_task.authority.to_string(),
                    task.authority.to_string(),
                ));
            }
        }

        self.db.insert_task(&db_task)?;
        self.task_queue
            .insert(db_task.clone(), Duration::from_millis(0));
        debug!("Registered task {} from context", task.id);

        Ok(())
    }

    pub fn unregister_task(&self, task_id: u64) -> TaskSchedulerResult<()> {
        self.db.remove_task(task_id)?;
        debug!("Removed task {} from database", task_id);

        Ok(())
    }

    async fn run(
        mut self,
        token: CancellationToken,
    ) -> TaskSchedulerResult<()> {
        let mut interval = tokio::time::interval(self.tick_interval);
        loop {
            select! {
                Some(task) = self.task_queue.next() => {
                    let task = task.get_ref();
                    self.task_queue_keys.remove(&task.id);
                    if let Err(e) = self.execute_task(task).await {
                        error!("Failed to execute task {}: {}", task.id, e);

                        // If any instruction fails, the task is cancelled
                        self.db.remove_task(task.id)?;
                        self.db.insert_failed_task(task.id, format!("{:?}", e))?;
                    }
                }
                _ = interval.tick() => {
                    // HACK: we deserialize the context on every tick avoid using geyser. This will be fixed once the channel to the transaction executor is implemented.
                    // Performance should not be too bad because the context should be small.
                    // https://github.com/magicblock-labs/magicblock-validator/issues/523

                    // Process any existing requests from the context
                    let Some(context_account) = self.bank.get_account(&TASK_CONTEXT_PUBKEY) else {
                        error!("Task context account not found");
                        return Err(TaskSchedulerError::TaskContextNotFound);
                    };

                    let task_context = bincode::deserialize::<TaskContext>(context_account.data()).unwrap_or_default();

                    if task_context.requests.is_empty() {
                        // Nothing to do because there are no requests in the context
                        continue;
                    }

                    match self.process_context_requests(&task_context.requests) {
                        Ok(errors) => {
                            if !errors.is_empty() {
                                warn!("Failed to process {} requests out of {}", errors.len(), task_context.requests.len());
                            }

                            // All requests were processed, reset the context
                            if let Err(e) =  self.process_transaction(vec![
                                InstructionUtils::process_tasks_instruction(
                                    &validator_authority_id(),
                                ),
                            ]).await {
                                error!("Failed to reset task context: {}", e);
                                return Err(e);
                            }
                            debug!("Processed {} requests", task_context.requests.len());
                        }
                        Err(e) => {
                            error!("Failed to process context requests: {}", e);
                            return Err(e);
                        }
                    }
                }
                _ = token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    fn remove_task_from_queue(&mut self, task_id: u64) {
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
