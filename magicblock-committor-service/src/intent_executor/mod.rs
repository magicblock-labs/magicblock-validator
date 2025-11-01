pub mod error;
pub(crate) mod intent_executor_factory;
pub mod single_stage_executor;
pub mod task_info_fetcher;
pub mod two_stage_executor;

use std::{ops::ControlFlow, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::future::try_join_all;
use log::{error, trace, warn};
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledBaseIntent,
    validator::validator_authority,
};
use magicblock_rpc_client::{
    utils::{
        decide_rpc_error_flow, map_magicblock_client_error,
        send_transaction_with_retries, SendErrorMapper, TransactionErrorMapper,
    },
    MagicBlockSendTransactionConfig, MagicBlockSendTransactionOutcome,
    MagicblockRpcClient,
};
use solana_pubkey::Pubkey;
use solana_sdk::{
    message::VersionedMessage,
    signature::{Keypair, Signature, Signer, SignerError},
    transaction::VersionedTransaction,
};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            IntentTransactionErrorMapper, InternalError,
            TransactionStrategyExecutionError,
        },
        single_stage_executor::SingleStageExecutor,
        task_info_fetcher::{ResetType, TaskInfoFetcher},
        two_stage_executor::TwoStageExecutor,
    },
    persist::{CommitStatus, CommitStatusSignatures, IntentPersister},
    tasks::{
        task_builder::{TaskBuilderError, TaskBuilderImpl, TasksBuilder},
        task_strategist::{
            TaskStrategist, TaskStrategistError, TransactionStrategy,
        },
        task_visitors::utility_visitor::TaskVisitorUtils,
        BaseTask, TaskType,
    },
    transaction_preparator::{
        error::TransactionPreparatorError, TransactionPreparator,
    },
    utils::persist_status_update_by_message_set,
};

#[derive(Clone, Copy, Debug)]
pub enum ExecutionOutput {
    // TODO: with arrival of challenge window remove SingleStage
    // Protocol requires 2 stage: Commit, Finalize
    // SingleStage - optimization for timebeing
    SingleStage(Signature),
    TwoStage {
        /// Commit stage signature
        commit_signature: Signature,
        /// Finalize stage signature
        finalize_signature: Signature,
    },
}

#[async_trait]
pub trait IntentExecutor: Send + Sync + 'static {
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        persister: Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput>;
}

pub struct IntentExecutorImpl<T, F> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
    task_info_fetcher: Arc<F>,
}

impl<T, F> IntentExecutorImpl<T, F>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    pub fn new(
        rpc_client: MagicblockRpcClient,
        transaction_preparator: T,
        task_info_fetcher: Arc<F>,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            rpc_client,
            transaction_preparator,
            task_info_fetcher,
        }
    }

    /// Checks if it is possible to unite Commit & Finalize stages in 1 transaction
    /// Returns corresponding `TransactionStrategy` if possible, otherwise `None`
    fn try_unite_tasks<P: IntentPersister>(
        commit_tasks: &[Box<dyn BaseTask>],
        finalize_task: &[Box<dyn BaseTask>],
        authority: &Pubkey,
        persister: &Option<P>,
    ) -> Result<Option<TransactionStrategy>, SignerError> {
        const MAX_UNITED_TASKS_LEN: usize = 22;

        // We can unite in 1 tx a lot of commits
        // but then there's a possibility of hitting CPI limit, aka
        // MaxInstructionTraceLengthExceeded error.
        // So we limit tasks len with 22 total tasks
        // In case this fails as well, it will be retried with TwoStage approach
        // on retry, once retries are introduced
        if commit_tasks.len() + finalize_task.len() > MAX_UNITED_TASKS_LEN {
            return Ok(None);
        }

        // Clone tasks since strategies applied to united case maybe suboptimal for regular one
        let mut commit_tasks = commit_tasks.to_owned();
        let finalize_task = finalize_task.to_owned();

        // Unite tasks to attempt running as single tx
        commit_tasks.extend(finalize_task);
        match TaskStrategist::build_strategy(commit_tasks, authority, persister)
        {
            Ok(strategy) => Ok(Some(strategy)),
            Err(TaskStrategistError::FailedToFitError) => Ok(None),
            Err(TaskStrategistError::SignerError(err)) => Err(err),
        }
    }

    async fn execute_inner<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        if base_intent.is_empty() {
            return Err(IntentExecutorError::EmptyIntentError);
        }

        // Update tasks status to Pending
        if let Some(pubkeys) = base_intent.get_committed_pubkeys() {
            let update_status = CommitStatus::Pending;
            persist_status_update_by_message_set(
                persister,
                base_intent.id,
                &pubkeys,
                update_status,
            );
        }

        // Build tasks for commit stage
        let commit_tasks = TaskBuilderImpl::commit_tasks(
            &self.task_info_fetcher,
            &base_intent,
            persister,
        )
        .await?;

        let committed_pubkeys = match base_intent.get_committed_pubkeys() {
            Some(value) => value,
            None => {
                // Standalone actions executed in single stage
                let strategy = TaskStrategist::build_strategy(
                    commit_tasks,
                    &self.authority.pubkey(),
                    persister,
                )?;
                return self
                    .single_stage_execution_flow(
                        base_intent,
                        strategy,
                        persister,
                    )
                    .await;
            }
        };

        let finalize_tasks = TaskBuilderImpl::finalize_tasks(
            &self.task_info_fetcher,
            &base_intent,
        )
        .await?;

        // See if we can squeeze them in one tx
        if let Some(single_tx_strategy) = Self::try_unite_tasks(
            &commit_tasks,
            &finalize_tasks,
            &self.authority.pubkey(),
            persister,
        )? {
            trace!("Executing intent in single stage");
            let output = self
                .single_stage_execution_flow(
                    base_intent,
                    single_tx_strategy,
                    persister,
                )
                .await?;

            Ok(output)
        } else {
            // Build strategy for Commit stage
            let commit_strategy = TaskStrategist::build_strategy(
                commit_tasks,
                &self.authority.pubkey(),
                persister,
            )?;

            // Build strategy for Finalize stage
            let finalize_strategy = TaskStrategist::build_strategy(
                finalize_tasks,
                &self.authority.pubkey(),
                persister,
            )?;

            trace!("Executing intent in two stages");
            let output = self
                .two_stage_execution_flow(
                    &committed_pubkeys,
                    commit_strategy,
                    finalize_strategy,
                    persister,
                )
                .await?;

            Ok(output)
        }
    }

    /// Starting execution from single stage
    // TODO(edwin): introduce recursion stop value in case of some bug?
    pub async fn single_stage_execution_flow<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        transaction_strategy: TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let mut junk = Vec::new();
        let res = SingleStageExecutor::new(self)
            .execute(base_intent, transaction_strategy, &mut junk, persister)
            .await;

        // Cleanup after intent
        // Note: in some cases it maybe critical to execute cleanup synchronously
        // Example: if commit nonces were invalid during execution
        // next intent could use wrongly initiated buffers by current intent
        let cleanup_futs = junk.iter().map(|to_cleanup| {
            self.transaction_preparator.cleanup_for_strategy(
                &self.authority,
                &to_cleanup.optimized_tasks,
                &to_cleanup.lookup_tables_keys,
            )
        });
        if let Err(err) = try_join_all(cleanup_futs).await {
            error!("Failed to cleanup after intent: {}", err);
        }

        res
    }

    pub async fn two_stage_execution_flow<P: IntentPersister>(
        &self,
        committed_pubkeys: &[Pubkey],
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let mut junk = Vec::new();
        let res = TwoStageExecutor::new(self)
            .execute(
                committed_pubkeys,
                commit_strategy,
                finalize_strategy,
                &mut junk,
                persister,
            )
            .await;

        // Cleanup after intent
        // Note: in some cases it maybe critical to execute cleanup synchronously
        // Example: if commit nonces were invalid during execution
        // next intent could use wrongly initiated buffers by current intent
        let cleanup_futs = junk.iter().map(|to_cleanup| {
            self.transaction_preparator.cleanup_for_strategy(
                &self.authority,
                &to_cleanup.optimized_tasks,
                &to_cleanup.lookup_tables_keys,
            )
        });
        if let Err(err) = try_join_all(cleanup_futs).await {
            error!("Failed to cleanup after intent: {}", err);
        }

        res
    }

    /// Handles out of sync commit id error, fixes current strategy
    /// Returns strategy to be cleaned up
    /// TODO(edwin): TransactionStrategy -> CleanuoStrategy or something, naming it confusing for something that is cleaned up
    async fn handle_commit_id_error(
        &self,
        committed_pubkeys: &[Pubkey],
        strategy: &mut TransactionStrategy,
    ) -> Result<TransactionStrategy, TaskBuilderError> {
        // This means that some Tasks out of sync with base layer commit ids
        // We reset TaskInfoFetcher for all committed accounts
        // We re-fetch them to fix out of sync tasks
        self.task_info_fetcher
            .reset(ResetType::Specific(committed_pubkeys));
        let commit_ids = self
            .task_info_fetcher
            .fetch_next_commit_ids(committed_pubkeys)
            .await
            .map_err(TaskBuilderError::CommitTasksBuildError)?;

        // Here we find the broken tasks and reset them
        // Broken tasks are prepared incorrectly so they have to be cleaned up
        let mut visitor = TaskVisitorUtils::GetCommitMeta(None);
        let mut to_cleanup = Vec::new();
        for task in strategy.optimized_tasks.iter_mut() {
            task.visit(&mut visitor);
            let TaskVisitorUtils::GetCommitMeta(Some(ref commit_meta)) =
                visitor
            else {
                continue;
            };

            let Some(commit_id) = commit_ids.get(&commit_meta.committed_pubkey)
            else {
                continue;
            };
            if commit_id == &commit_meta.commit_id {
                continue;
            }

            // Handle invalid tasks
            to_cleanup.push(task.clone());
            task.reset_commit_id(*commit_id);
        }

        let old_alts = strategy.dummy_revaluate_alts(&self.authority.pubkey());
        Ok(TransactionStrategy {
            optimized_tasks: to_cleanup,
            lookup_tables_keys: old_alts,
        })
    }

    /// Handles actions error, stripping away actions
    /// Returns [`TransactionStrategy`] to be cleaned up
    fn handle_actions_error(
        &self,
        strategy: &mut TransactionStrategy,
    ) -> TransactionStrategy {
        // Strip away actions
        let (optimized_tasks, action_tasks) = strategy
            .optimized_tasks
            .drain(..)
            .partition(|el| el.task_type() != TaskType::Action);
        strategy.optimized_tasks = optimized_tasks;

        let old_alts = strategy.dummy_revaluate_alts(&self.authority.pubkey());

        TransactionStrategy {
            optimized_tasks: action_tasks,
            lookup_tables_keys: old_alts,
        }
    }

    /// Handle CPI limit error, splits single strategy flow into 2
    /// Returns Commit stage strategy, Finalize stage strategy and strategy to clean up
    fn handle_cpi_limit_error(
        &self,
        strategy: TransactionStrategy,
    ) -> (
        TransactionStrategy,
        TransactionStrategy,
        TransactionStrategy,
    ) {
        // We encountered error "Max instruction trace length exceeded"
        // All the tasks a prepared to be executed at this point
        // We attempt Two stages commit flow, need to split tasks up
        let (commit_stage_tasks, finalize_stage_tasks): (Vec<_>, Vec<_>) =
            strategy
                .optimized_tasks
                .into_iter()
                .partition(|el| el.task_type() == TaskType::Commit);

        let commit_alt_pubkeys = if strategy.lookup_tables_keys.is_empty() {
            vec![]
        } else {
            TaskStrategist::collect_lookup_table_keys(
                &self.authority.pubkey(),
                &commit_stage_tasks,
            )
        };
        let commit_strategy = TransactionStrategy {
            optimized_tasks: commit_stage_tasks,
            lookup_tables_keys: commit_alt_pubkeys,
        };

        let finalize_alt_pubkeys = if strategy.lookup_tables_keys.is_empty() {
            vec![]
        } else {
            TaskStrategist::collect_lookup_table_keys(
                &self.authority.pubkey(),
                &finalize_stage_tasks,
            )
        };
        let finalize_strategy = TransactionStrategy {
            optimized_tasks: finalize_stage_tasks,
            lookup_tables_keys: finalize_alt_pubkeys,
        };

        // We clean up only ALTs
        let to_cleanup = TransactionStrategy {
            optimized_tasks: vec![],
            lookup_tables_keys: strategy.lookup_tables_keys,
        };

        (commit_strategy, finalize_strategy, to_cleanup)
    }

    /// Shared helper for sending transactions
    async fn send_prepared_message(
        &self,
        mut prepared_message: VersionedMessage,
    ) -> IntentExecutorResult<MagicBlockSendTransactionOutcome, InternalError>
    {
        let latest_blockhash = self.rpc_client.get_latest_blockhash().await?;
        match &mut prepared_message {
            VersionedMessage::V0(value) => {
                value.recent_blockhash = latest_blockhash;
            }
            VersionedMessage::Legacy(value) => {
                warn!("TransactionPreparator v1 does not use Legacy message");
                value.recent_blockhash = latest_blockhash;
            }
        };

        let transaction = VersionedTransaction::try_new(
            prepared_message,
            &[&self.authority],
        )?;
        let result = self
            .rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;

        Ok(result)
    }

    /// Flushes result into presistor
    /// The result will be propagated down to callers
    fn persist_result<P: IntentPersister>(
        persistor: &P,
        result: &IntentExecutorResult<ExecutionOutput>,
        message_id: u64,
        pubkeys: &[Pubkey],
    ) {
        let update_status = match result {
            Ok(value) => {
                let signatures = match *value {
                    ExecutionOutput::SingleStage(signature) => {
                        CommitStatusSignatures {
                            commit_stage_signature: signature,
                            finalize_stage_signature: Some(signature),
                        }
                    }
                    ExecutionOutput::TwoStage {
                        commit_signature,
                        finalize_signature,
                    } => CommitStatusSignatures {
                        commit_stage_signature: commit_signature,
                        finalize_stage_signature: Some(finalize_signature),
                    },
                };
                let update_status = CommitStatus::Succeeded(signatures);
                persist_status_update_by_message_set(
                    persistor,
                    message_id,
                    pubkeys,
                    update_status,
                );

                if let Err(err) =
                    persistor.finalize_base_intent(message_id, *value)
                {
                    log::error!("Failed to persist ExecutionOutput: {}", err);
                }

                return;
            }
            Err(IntentExecutorError::CommitIDError(_, _))
            | Err(IntentExecutorError::ActionsError(_, _))
            | Err(IntentExecutorError::CpiLimitError(_, _)) => None,
            Err(IntentExecutorError::EmptyIntentError)
            | Err(IntentExecutorError::FailedToFitError)
            | Err(IntentExecutorError::TaskBuilderError(_))
            | Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::SignerError(_),
            ))
            | Err(IntentExecutorError::FailedFinalizePreparationError(
                TransactionPreparatorError::SignerError(_),
            )) => Some(CommitStatus::Failed),
            Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::FailedToFitError,
            )) => Some(CommitStatus::PartOfTooLargeBundleToProcess),
            Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::DeliveryPreparationError(_),
            )) => {
                // Intermediate commit preparation progress recorded by DeliveryPreparator
                None
            }
            Err(IntentExecutorError::FailedToCommitError {
                err: _,
                signature,
            }) => {
                // Commit is a single TX, so if it fails, all of commited accounts marked FailedProcess
                let status_signature =
                    signature.map(|sig| CommitStatusSignatures {
                        commit_stage_signature: sig,
                        finalize_stage_signature: None,
                    });
                Some(CommitStatus::FailedProcess(status_signature))
            }
            Err(IntentExecutorError::FailedFinalizePreparationError(_)) => {
                // Not supported in persistor
                None
            }
            Err(IntentExecutorError::FailedToFinalizeError {
                err: _,
                commit_signature,
                finalize_signature,
            }) => {
                // Finalize is a single TX, so if it fails, all of commited accounts marked FailedFinalize
                let update_status =
                    if let Some(commit_signature) = commit_signature {
                        let signatures = CommitStatusSignatures {
                            commit_stage_signature: *commit_signature,
                            finalize_stage_signature: *finalize_signature,
                        };
                        CommitStatus::FailedFinalize(signatures)
                    } else {
                        CommitStatus::FailedProcess(None)
                    };

                Some(update_status)
            }
            Err(IntentExecutorError::SignerError(_)) => {
                Some(CommitStatus::Failed)
            }
        };

        if let Some(update_status) = update_status {
            persist_status_update_by_message_set(
                persistor,
                message_id,
                pubkeys,
                update_status,
            );
        }
    }

    pub async fn prepare_and_execute_strategy<P: IntentPersister>(
        &self,
        transaction_strategy: &mut TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<
        IntentExecutorResult<Signature, TransactionStrategyExecutionError>,
        TransactionPreparatorError,
    > {
        // Prepare message
        let prepared_message = self
            .transaction_preparator
            .prepare_for_strategy(
                &self.authority,
                transaction_strategy,
                persister,
            )
            .await?;

        // Execute strategy
        let execution_result = self
            .execute_message_with_retries(
                prepared_message,
                &transaction_strategy.optimized_tasks,
            )
            .await;

        Ok(execution_result)
    }

    async fn execute_message_with_retries(
        &self,
        prepared_message: VersionedMessage,
        tasks: &[Box<dyn BaseTask>],
    ) -> IntentExecutorResult<Signature, TransactionStrategyExecutionError>
    {
        struct IntentErrorMapper<TxMap> {
            transaction_error_mapper: TxMap,
        }
        impl<TxMap> SendErrorMapper<InternalError> for IntentErrorMapper<TxMap>
        where
            TxMap: TransactionErrorMapper<
                ExecutionError = TransactionStrategyExecutionError,
            >,
        {
            type ExecutionError = TransactionStrategyExecutionError;
            fn map(&self, error: InternalError) -> Self::ExecutionError {
                match error {
                    InternalError::MagicBlockRpcClientError(err) => {
                        map_magicblock_client_error(
                            &self.transaction_error_mapper,
                            err,
                        )
                    }
                    err => {
                        TransactionStrategyExecutionError::InternalError(err)
                    }
                }
            }

            fn decide_flow(
                err: &Self::ExecutionError,
            ) -> ControlFlow<(), Duration> {
                match err {
                    TransactionStrategyExecutionError::InternalError(
                        InternalError::MagicBlockRpcClientError(err),
                    ) => decide_rpc_error_flow(err),
                    _ => ControlFlow::Break(()),
                }
            }
        }

        const RETRY_FOR: Duration = Duration::from_secs(2 * 60);
        const MIN_ATTEMPTS: usize = 3;

        // Send with retries
        let send_error_mapper = IntentErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper { tasks },
        };
        let attempt = || async {
            self.send_prepared_message(prepared_message.clone()).await
        };
        send_transaction_with_retries(
            attempt,
            send_error_mapper,
            |i, elapsed| !(elapsed < RETRY_FOR || i < MIN_ATTEMPTS),
        )
        .await
    }
}

#[async_trait]
impl<T, C> IntentExecutor for IntentExecutorImpl<T, C>
where
    T: TransactionPreparator,
    C: TaskInfoFetcher,
{
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        persister: Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let message_id = base_intent.id;
        let is_undelegate = base_intent.is_undelegate();
        let pubkeys = base_intent.get_committed_pubkeys();

        let result = self.execute_inner(base_intent, &persister).await;
        if let Some(pubkeys) = pubkeys {
            // Reset TaskInfoFetcher, as cache could become invalid
            if result.is_err() || is_undelegate {
                self.task_info_fetcher.reset(ResetType::Specific(&pubkeys));
            }

            // Write result of intent into Persister
            Self::persist_result(&persister, &result, message_id, &pubkeys);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use solana_pubkey::Pubkey;

    use crate::{
        intent_execution_manager::intent_scheduler::create_test_intent,
        intent_executor::{
            task_info_fetcher::{
                ResetType, TaskInfoFetcher, TaskInfoFetcherResult,
            },
            IntentExecutorImpl,
        },
        persist::IntentPersisterImpl,
        tasks::task_builder::{TaskBuilderImpl, TasksBuilder},
        transaction_preparator::TransactionPreparatorImpl,
    };

    struct MockInfoFetcher;
    #[async_trait::async_trait]
    impl TaskInfoFetcher for MockInfoFetcher {
        async fn fetch_next_commit_ids(
            &self,
            pubkeys: &[Pubkey],
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
        }

        async fn fetch_rent_reimbursements(
            &self,
            pubkeys: &[Pubkey],
        ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
            Ok(pubkeys.iter().map(|_| Pubkey::new_unique()).collect())
        }

        fn peek_commit_id(&self, _pubkey: &Pubkey) -> Option<u64> {
            Some(0)
        }

        fn reset(&self, _: ResetType) {}
    }

    #[tokio::test]
    async fn test_try_unite() {
        let pubkey = [Pubkey::new_unique()];
        let intent = create_test_intent(0, &pubkey);

        let info_fetcher = Arc::new(MockInfoFetcher);
        let commit_task = TaskBuilderImpl::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();
        let finalize_task =
            TaskBuilderImpl::finalize_tasks(&info_fetcher, &intent)
                .await
                .unwrap();

        let result = IntentExecutorImpl::<
            TransactionPreparatorImpl,
            MockInfoFetcher,
        >::try_unite_tasks(
            &commit_task,
            &finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
        );

        let strategy = result.unwrap().unwrap();
        assert!(strategy.lookup_tables_keys.is_empty());
    }
}
