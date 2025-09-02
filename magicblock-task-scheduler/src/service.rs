use std::{collections::HashSet, path::Path, sync::Arc};

use futures_util::StreamExt;
use log::*;
use magicblock_bank::bank::Bank;
use magicblock_config::TaskSchedulerConfig;
use magicblock_program::{
    CancelTaskRequest, CrankTask, ScheduleTaskRequest, TaskContext,
    TaskRequest, TASK_CONTEXT_PUBKEY,
};
use solana_sdk::{
    account::ReadableAccount,
    message::Message,
    signature::Keypair,
    signer::Signer,
    transaction::{SanitizedTransaction, Transaction},
};
use solana_svm::transaction_processor::ExecutionRecordingConfig;
use solana_timings::ExecuteTimings;
use tokio::{select, time::Duration};
use tokio_util::{sync::CancellationToken, time::DelayQueue};

use crate::{
    db::{DbTask, FailedScheduling, FailedTask, SchedulerDatabase},
    errors::TaskSchedulerError,
};

pub struct TaskSchedulerService {
    db: SchedulerDatabase,
    bank: Arc<Bank>,
    tick_interval: Duration,
    task_queue: DelayQueue<DbTask>,
    pending_cancellations: HashSet<u64>,
}

impl TaskSchedulerService {
    pub fn start(
        path: &Path,
        config: &TaskSchedulerConfig,
        bank: Arc<Bank>,
        token: CancellationToken,
    ) -> Result<tokio::task::JoinHandle<()>, TaskSchedulerError> {
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
            pending_cancellations: HashSet::new(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        debug!("Task scheduler started at {}", now);
        for task in tasks {
            let next_execution =
                task.last_execution_millis + task.execution_interval_millis;
            let timeout = Duration::from_millis(if next_execution > now {
                (next_execution - now) as u64
            } else {
                0
            });
            service.task_queue.insert(task, timeout);
        }

        Ok(tokio::spawn(service.run(token)))
    }

    fn process_context_requests(
        &mut self,
        task_context: &mut TaskContext,
    ) -> Result<usize, TaskSchedulerError> {
        let requests = task_context.get_all_requests();
        let mut ids = Vec::new();

        for request in requests {
            trace!("Processing task scheduling request: {request:?}");
            let id = match request {
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
                    }
                    schedule_request.id
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
                    }
                    cancel_request.task_id
                }
            };

            ids.push(id);
        }

        for id in &ids {
            task_context.remove_request(*id);
        }

        Ok(ids.len())
    }

    fn process_schedule_request(
        &mut self,
        schedule_request: &ScheduleTaskRequest,
    ) -> Result<(), TaskSchedulerError> {
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
    ) -> Result<(), TaskSchedulerError> {
        // Record ID to prevent the task from being executed
        self.pending_cancellations.insert(cancel_request.task_id);
        // Remove task from database
        self.unregister_task(cancel_request.task_id)?;
        trace!(
            "Processed cancel request for task {}",
            cancel_request.task_id
        );
        Ok(())
    }

    fn execute_task(
        &mut self,
        task: &DbTask,
    ) -> Result<(), TaskSchedulerError> {
        trace!("Executing task {}", task.id);

        if self.pending_cancellations.remove(&task.id) {
            warn!("Task {} is pending cancellation", task.id);
            self.db.unschedule_task(task.id)?;
            return Ok(());
        }

        // Execute unsigned transactions
        let blockhash = self.bank.last_blockhash();
        let sanitized_transactions = match task
            .instructions
            .iter()
            .map(|ix| {
                // Using a fake payer to make sure the transaction has a new signature.
                // Otherwise, the transaction will error saying already processed.
                let fake_payer = Keypair::new();
                let mut tx =
                    Transaction::new_unsigned(Message::new_with_blockhash(
                        &[ix.clone()],
                        Some(&fake_payer.pubkey()),
                        &blockhash,
                    ));
                tx.partial_sign(&[fake_payer], blockhash);
                SanitizedTransaction::try_from_legacy_transaction(
                    tx,
                    &Default::default(),
                )
            })
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(transactions) => transactions,
            Err(e) => {
                error!("Failed to sanitize transactions: {}", e);
                return Err(TaskSchedulerError::Transaction(e));
            }
        };
        let batch = self.bank.prepare_sanitized_batch(&sanitized_transactions);
        let (output, _balances) =
            self.bank.load_execute_and_commit_transactions(
                &batch,
                false,
                ExecutionRecordingConfig::new_single_setting(true),
                &mut ExecuteTimings::default(),
                None,
            );

        // If any instruction fails, the task is cancelled
        for result in output {
            if let Err(e) = result.and_then(|tx| tx.status) {
                error!("Task {} failed to execute: {}", task.id, e);
                self.db.insert_failed_task(&FailedTask { id: task.id })?;
                self.db.unschedule_task(task.id)?;
                return Err(TaskSchedulerError::Transaction(e));
            }
        }

        if task.executions_left > 1 {
            // Reschedule the task
            let new_task = DbTask {
                executions_left: task.executions_left - 1,
                ..task.clone()
            };
            self.task_queue.insert(
                new_task,
                Duration::from_millis(task.execution_interval_millis as u64),
            );
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
    ) -> Result<(), TaskSchedulerError> {
        let db_task = DbTask {
            id: task.id,
            instructions: task.instructions.clone(),
            authority: task.authority,
            execution_interval_millis: task.execution_interval_millis,
            executions_left: task.n_executions,
            last_execution_millis: 0,
        };

        self.db.insert_task(&db_task)?;
        self.task_queue
            .insert(db_task.clone(), Duration::from_millis(0));
        debug!("Registered task {} from context", task.id);

        Ok(())
    }

    pub fn unregister_task(
        &self,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        self.db.remove_task(task_id)?;
        debug!("Removed task {} from context", task_id);

        Ok(())
    }

    async fn run(mut self, token: CancellationToken) {
        let mut interval = tokio::time::interval(self.tick_interval);
        loop {
            select! {
                Some(task) = self.task_queue.next() => {
                    if let Err(e) = self.execute_task(task.get_ref()) {
                        error!("Failed to execute task {}: {}", task.get_ref().id, e);
                    }
                }
                _ = interval.tick() => {
                    // HACK: we deserialize the context on every tick avoid using geyser.
                    // This will be fixed once the channel to the transaction executor is implemented.
                    // Performance should not be too bad because the context should be small.

                    // Process any existing requests from the context
                    let Some(mut context_account) = self.bank.get_account(&TASK_CONTEXT_PUBKEY) else {
                        error!("Task context account not found");
                        continue;
                    };

                    let Ok(mut task_context) =
                        bincode::deserialize::<TaskContext>(context_account.data()) else {
                        error!("Invalid task context account");
                        continue;
                    };

                    match self.process_context_requests(&mut task_context) {
                        Ok(n) if n > 0 => {
                            // Write the updated context back to the account
                            let Ok(serialized) = bincode::serialize(&task_context) else {
                                error!("Failed to serialize task context");
                                continue;
                            };
                            context_account.set_data(serialized);
                            self.bank.store_account(TASK_CONTEXT_PUBKEY, context_account);
                        }
                        Err(e) => {
                            error!("Failed to process context requests: {}", e);
                            continue;
                        }
                        _ => {}
                    }
                }
                _ = token.cancelled() => {
                    break;
                }
            }
        }
    }
}
