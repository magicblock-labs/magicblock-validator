use std::sync::Arc;

use log::*;
use magicblock_bank::bank::Bank;
use magicblock_config::TaskSchedulerConfig;
use magicblock_program::{
    CancelTaskRequest, ScheduleTaskRequest, Task, TaskContext, TaskRequest,
    TASK_CONTEXT_PUBKEY,
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
use tokio_util::sync::CancellationToken;

use crate::{
    db::{DbTask, SchedulerDatabase},
    errors::TaskSchedulerError,
};

pub struct TaskSchedulerService {
    db: SchedulerDatabase,
    bank: Arc<Bank>,
    tick_interval: Duration,
}

impl TaskSchedulerService {
    pub fn start(
        config: &TaskSchedulerConfig,
        bank: Arc<Bank>,
        token: CancellationToken,
    ) -> Result<tokio::task::JoinHandle<()>, TaskSchedulerError> {
        debug!("Initializing task scheduler service");
        if config.reset_db {
            if let Err(e) = std::fs::remove_file(&config.db_path) {
                warn!("Failed to remove database file: {}", e);
            }
        }
        let db = SchedulerDatabase::new(&config.db_path)?;

        Ok(tokio::spawn(
            Self {
                db,
                bank,
                tick_interval: Duration::from_millis(config.millis_per_tick),
            }
            .run(token),
        ))
    }

    fn process_context_requests(
        &self,
        task_context: &mut TaskContext,
    ) -> Result<usize, TaskSchedulerError> {
        let requests = task_context.get_all_requests();
        let mut ids = Vec::new();

        for request in requests {
            debug!("Processing task scheduling request: {request:?}");
            let id = match request {
                TaskRequest::Schedule(schedule_request) => {
                    if let Err(e) =
                        self.process_schedule_request(schedule_request)
                    {
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
        &self,
        schedule_request: &ScheduleTaskRequest,
    ) -> Result<(), TaskSchedulerError> {
        // Convert request to task and register in database
        let task = Task::from(schedule_request);
        self.register_task(&task)?;
        debug!(
            "Processed schedule request for task {}",
            schedule_request.id
        );
        Ok(())
    }

    fn process_cancel_request(
        &self,
        cancel_request: &CancelTaskRequest,
    ) -> Result<(), TaskSchedulerError> {
        // Remove task from database
        self.unregister_task(cancel_request.task_id)?;
        debug!(
            "Processed cancel request for task {}",
            cancel_request.task_id
        );
        Ok(())
    }

    fn tick(&self) -> Result<(), TaskSchedulerError> {
        let current_time = chrono::Utc::now().timestamp_millis();

        // Get executable tasks
        let executable_tasks = self.db.get_executable_tasks(current_time)?;

        for task in executable_tasks {
            if let Err(e) = self.execute_task(&task) {
                error!("Failed to execute task {}: {}", task.id, e);

                // Unschedule the task
                // It is not removed to avoid re-scheduling the task
                self.db.unschedule_task(task.id)?;
            }
        }

        Ok(())
    }

    fn execute_task(&self, task: &DbTask) -> Result<(), TaskSchedulerError> {
        debug!("Executing task {}", task.id);

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
                return Err(TaskSchedulerError::SanitizeTransactions(
                    e.to_string(),
                ));
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

        for result in output {
            if let Err(e) = result.and_then(|tx| tx.status) {
                return Err(TaskSchedulerError::TaskExecution(e.to_string()));
            }
        }

        // Update task in database
        let next_execution = if task.executions_left > 1 {
            task.next_execution_millis + task.period_millis
        } else {
            // Task completed
            0
        };

        self.db
            .update_task_after_execution(task.id, next_execution)?;

        Ok(())
    }

    pub fn register_task(&self, task: &Task) -> Result<(), TaskSchedulerError> {
        let db_task = DbTask {
            id: task.id,
            instructions: task.instructions.clone(),
            authority: task.authority,
            period_millis: task.period_millis,
            executions_left: task.n_executions,
            next_execution_millis: 0, // Run ASAP
        };

        self.db.insert_task(&db_task)?;
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

    async fn run(self, token: CancellationToken) {
        let mut interval = tokio::time::interval(self.tick_interval);
        loop {
            select! {
                _ = interval.tick() => {
                    if let Err(e) = self.tick() {
                        error!("Error in scheduler tick: {}", e);
                    }

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
