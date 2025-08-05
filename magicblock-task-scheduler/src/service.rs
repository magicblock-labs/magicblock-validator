use std::sync::Arc;

use log::*;
use magicblock_bank::bank::Bank;
use magicblock_config::TaskSchedulerConfig;
use magicblock_geyser_plugin::{
    grpc_messages::Message as GrpcMessage, types::GeyserMessage,
};
use magicblock_program::{Task, TaskContext, TASK_CONTEXT_PUBKEY};
use solana_sdk::{
    account::ReadableAccount,
    message::Message,
    signature::Keypair,
    signer::Signer,
    transaction::{SanitizedTransaction, Transaction},
};
use solana_svm::transaction_processor::ExecutionRecordingConfig;
use solana_timings::ExecuteTimings;
use tokio::{select, sync::mpsc::Receiver, time::Duration};
use tokio_util::sync::CancellationToken;

use crate::{
    db::{DbTask, SchedulerDatabase},
    errors::TaskSchedulerError,
};

pub struct TaskSchedulerService {
    db: SchedulerDatabase,
    bank: Arc<Bank>,
    tick_interval: Duration,
    started: bool,
}

impl TaskSchedulerService {
    pub fn new(
        config: &TaskSchedulerConfig,
        bank: Arc<Bank>,
    ) -> Result<Self, TaskSchedulerError> {
        if config.reset_db {
            std::fs::remove_file(&config.db_path)?;
        }
        let db = SchedulerDatabase::new(&config.db_path)?;

        // Register tasks from the context
        if let Some(context_account) = bank.get_account(&TASK_CONTEXT_PUBKEY) {
            let task_context: TaskContext =
                bincode::deserialize(context_account.data())?;
            for task in task_context.tasks.values() {
                if let Err(e) = Self::register_task(&db, task) {
                    // This happens if the task is already registered
                    error!("Failed to register task {}: {}", task.id, e);
                }
            }
        }

        Ok(Self {
            db,
            bank,
            tick_interval: Duration::from_millis(config.millis_per_tick),
            started: false,
        })
    }

    pub async fn start(
        &mut self,
        mut context_sub: Receiver<GeyserMessage>,
        token: CancellationToken,
    ) -> Result<(), TaskSchedulerError> {
        if self.started {
            return Err(TaskSchedulerError::AlreadyStarted);
        }

        self.started = true;
        let mut interval = tokio::time::interval(self.tick_interval);
        let db = self.db.clone();
        let bank = self.bank.clone();

        tokio::spawn(async move {
            loop {
                select! {
                    _ = interval.tick() => {
                        if let Err(e) = Self::tick(&db, &bank) {
                            error!("Error in scheduler tick: {}", e);
                        }
                    }
                    notification = context_sub.recv() => {
                        match notification {
                            Some(ref notification) => {
                                let GrpcMessage::Account(account) = notification.as_ref() else {
                                    continue;
                                };

                                let Ok(task_context) = bincode::deserialize::<TaskContext>(&account.account.data) else {
                                    continue;
                                };

                                let Ok(task_ids) = db.get_task_ids() else {
                                    continue;
                                };

                                debug!("Task context account updated: {:?}", task_context.tasks);

                                let tasks_to_register = task_context.tasks.values().filter(|task| !task_ids.contains(&task.id));
                                let tasks_to_unregister = task_ids.iter().filter(|id| !task_context.tasks.contains_key(id));

                                for task in tasks_to_register {
                                    if let Err(e) = Self::register_task(&db, task) {
                                        error!("Failed to register task {}: {}", task.id, e);
                                    }
                                }

                                for task_id in tasks_to_unregister {
                                    if let Err(e) = Self::unregister_task(&db, *task_id) {
                                        error!("Failed to unregister task {}: {}", task_id, e);
                                    }
                                }
                            }
                            None => {
                                debug!("Task scheduler context subscription closed");
                                break;
                            }
                        }
                    }
                    _ = token.cancelled() => {
                        debug!("Task scheduler cancelled");
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    pub fn shutdown(&mut self) {
        self.started = false;
        // The spawned task will be cancelled when the token is cancelled
    }

    fn tick(
        db: &SchedulerDatabase,
        bank: &Arc<Bank>,
    ) -> Result<(), TaskSchedulerError> {
        let current_time = chrono::Utc::now().timestamp_millis();

        // Get executable tasks
        let executable_tasks = db.get_executable_tasks(current_time)?;

        for task in executable_tasks {
            if let Err(e) = Self::execute_task(bank, db, &task) {
                error!("Failed to execute task {}: {}", task.id, e);

                // Unschedule the task
                // It is not removed to avoid re-scheduling the task
                db.unschedule_task(task.id)?;
            }
        }

        Ok(())
    }

    fn execute_task(
        bank: &Arc<Bank>,
        db: &SchedulerDatabase,
        task: &DbTask,
    ) -> Result<(), TaskSchedulerError> {
        // Execute unsigned transactions
        let blockhash = bank.last_blockhash();
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
        let batch = bank.prepare_sanitized_batch(&sanitized_transactions);
        let (output, _balances) = bank.load_execute_and_commit_transactions(
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

        db.update_task_after_execution(task.id, next_execution)?;

        Ok(())
    }

    pub fn register_task(
        db: &SchedulerDatabase,
        task: &Task,
    ) -> Result<(), TaskSchedulerError> {
        let db_task = DbTask {
            id: task.id,
            instructions: task.instructions.clone(),
            authority: task.authority,
            period_millis: task.period_millis,
            executions_left: task.n_executions,
            next_execution_millis: 0, // Run ASAP
        };
        db.insert_task(&db_task)?;
        debug!("Registered task {}", task.id);
        Ok(())
    }

    pub fn unregister_task(
        db: &SchedulerDatabase,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        db.remove_task(task_id)?;
        debug!("Unregistered task {}", task_id);
        Ok(())
    }
}

impl Drop for TaskSchedulerService {
    fn drop(&mut self) {
        if self.started {
            // If we're being dropped while started, try to shut down gracefully
            self.shutdown();
        }
    }
}
