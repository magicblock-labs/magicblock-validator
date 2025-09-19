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
use magicblock_bank::bank::Bank;
use magicblock_config::TaskSchedulerConfig;
use magicblock_program::{
    instruction_utils::InstructionUtils,
    validator::{validator_authority, validator_authority_id},
    CancelTaskRequest, CrankTask, ScheduleTaskRequest, TaskContext,
    TaskRequest, TASK_CONTEXT_PUBKEY,
};
use solana_sdk::{
    account::{ReadableAccount, WritableAccount},
    instruction::Instruction,
    message::Message,
    pubkey::Pubkey,
    transaction::{SanitizedTransaction, Transaction},
};
use solana_svm::{
    transaction_commit_result::TransactionCommitResult,
    transaction_processor::ExecutionRecordingConfig,
};
use solana_timings::ExecuteTimings;
use tokio::{select, time::Duration};
use tokio_util::{
    sync::CancellationToken,
    time::{delay_queue::Key, DelayQueue},
};

use crate::{
    db::{DbTask, FailedScheduling, FailedTask, SchedulerDatabase},
    errors::{TaskSchedulerError, TaskSchedulerResult},
};

const MEMO_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

pub struct TaskSchedulerService {
    /// Database for persisting tasks
    db: SchedulerDatabase,
    /// Bank for executing tasks
    bank: Arc<Bank>,
    /// Interval at which the task scheduler will check for requests in the context
    tick_interval: Duration,
    /// Queue of tasks to execute
    task_queue: DelayQueue<DbTask>,
    /// Map of task IDs to their corresponding keys in the task queue
    task_queue_keys: HashMap<u64, Key>,
    /// Counter used to make each transaction unique
    tx_counter: AtomicU64,
}

impl TaskSchedulerService {
    pub fn start(
        path: &Path,
        config: &TaskSchedulerConfig,
        bank: Arc<Bank>,
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
        task_context: &mut TaskContext,
    ) -> TaskSchedulerResult<Vec<TaskSchedulerError>> {
        let requests = &task_context.requests;
        let mut result = Vec::with_capacity(requests.len());
        for request in requests {
            trace!("Processing task scheduling request: {request:?}");
            match request {
                TaskRequest::Schedule(schedule_request) => {
                    if let Err(e) =
                        self.process_schedule_request(schedule_request)
                    {
                        self.db.insert_failed_scheduling(
                            &FailedScheduling {
                                id: schedule_request.id,
                            },
                        )?;
                        error!(
                            "Failed to process schedule request {}: {}",
                            schedule_request.id, e
                        );
                        result.push(e);
                    }
                }
                TaskRequest::Cancel(cancel_request) => {
                    if let Err(e) = self.process_cancel_request(cancel_request)
                    {
                        self.db.insert_failed_scheduling(
                            &FailedScheduling {
                                id: cancel_request.task_id,
                            },
                        )?;
                        error!(
                            "Failed to process cancel request for task {}: {}",
                            cancel_request.task_id, e
                        );
                        result.push(e);
                    }
                }
            };
        }

        Ok(result)
    }

    fn process_schedule_request(
        &mut self,
        schedule_request: &ScheduleTaskRequest,
    ) -> TaskSchedulerResult<()> {
        // Convert request to task and register in database
        let task = CrankTask::from(schedule_request);
        self.register_task(&task)?;
        trace!(
            "Processed schedule request for task {}",
            schedule_request.id
        );
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
        trace!(
            "Processed cancel request for task {}",
            cancel_request.task_id
        );
        Ok(())
    }

    fn execute_task(&mut self, task: &DbTask) -> TaskSchedulerResult<()> {
        trace!("Executing task {}", task.id);

        let output = self.process_transaction(task.instructions.clone())?;

        // If any instruction fails, the task is cancelled
        for result in output {
            if let Err(e) = result.and_then(|tx| tx.status) {
                error!("Task {} failed to execute: {}", task.id, e);
                self.db.insert_failed_task(&FailedTask { id: task.id })?;
                self.db.remove_task(task.id)?;
                return Err(TaskSchedulerError::Transaction(e));
            }
        }

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
        trace!(
            "Task {} has {} executions left",
            task.id,
            task.executions_left
        );

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
                    if let Err(e) = self.execute_task(task) {
                        error!("Failed to execute task {}: {}", task.id, e);
                    }
                }
                _ = interval.tick() => {
                    // HACK: we deserialize the context on every tick avoid using geyser.
                    // This will be fixed once the channel to the transaction executor is implemented.
                    // Performance should not be too bad because the context should be small.
                    // https://github.com/magicblock-labs/magicblock-validator/issues/523

                    // Process any existing requests from the context
                    let Some(mut context_account) = self.bank.get_account(&TASK_CONTEXT_PUBKEY) else {
                        error!("Task context account not found");
                        return Err(TaskSchedulerError::TaskContextNotFound);
                    };

                    let Ok(mut task_context) =
                        bincode::deserialize::<TaskContext>(context_account.data_as_mut_slice()) else {
                        error!("Invalid task context account");
                        return Err(TaskSchedulerError::ContextDeserialization(context_account.data().to_vec()));
                    };

                    trace!("Task context deserialized: {:?}", task_context);

                    match self.process_context_requests(&mut task_context) {
                        Ok(result) if result.is_empty() => {
                            if task_context.requests.is_empty() {
                                // Nothing to do because there are no requests in the context
                                continue;
                            }

                            // All requests were processed successfully, reset the context
                            // We need to freeze the bank to avoid race conditions
                            // This is done with an instruction to the magic program.
                            // It avoids race conditions with write access to accountsDb
                            trace!("Resetting task context");
                            let output = self.process_transaction(vec![
                                InstructionUtils::process_tasks_instruction(
                                    &validator_authority_id(),
                                ),
                            ])?;
                            for result in output {
                                if let Err(e) = result.and_then(|tx| tx.status) {
                                    error!("Failed to reset task context: {}", e);
                                    return Err(TaskSchedulerError::Transaction(e));
                                }
                            }
                        }
                        Ok(result) => {
                            for error in &result {
                                error!("Failed to process some context requests: {:?}", error);
                            }
                            return Err(TaskSchedulerError::SchedulingRequests(result));
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

    fn process_transaction(
        &self,
        instructions: Vec<Instruction>,
    ) -> TaskSchedulerResult<Vec<TransactionCommitResult>> {
        // Execute unsigned transactions
        // We prepend a noop instruction to make each transaction unique.
        let blockhash = self.bank.last_blockhash();
        let noop_instruction = Instruction::new_with_bytes(
            MEMO_PROGRAM_ID,
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

        // TODO: transaction should be sent to the transaction executor.
        // This is a work in progress and this should be updated once implemented.
        // https://github.com/magicblock-labs/magicblock-validator/issues/523
        let sanitized_transaction =
            match SanitizedTransaction::try_from_legacy_transaction(
                tx,
                &Default::default(),
            ) {
                Ok(tx) => [tx],
                Err(e) => {
                    error!("Failed to sanitize transaction: {}", e);
                    return Err(TaskSchedulerError::Transaction(e));
                }
            };
        let batch = self.bank.prepare_sanitized_batch(&sanitized_transaction);
        let (output, _balances) =
            self.bank.load_execute_and_commit_transactions(
                &batch,
                false,
                ExecutionRecordingConfig::new_single_setting(true),
                &mut ExecuteTimings::default(),
                None,
            );

        Ok(output)
    }
}
