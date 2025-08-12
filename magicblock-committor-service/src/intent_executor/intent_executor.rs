use std::sync::Arc;

use log::{info, warn};
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledBaseIntent,
    validator::validator_authority,
};
use magicblock_rpc_client::{
    MagicBlockSendTransactionConfig, MagicblockRpcClient,
};
use solana_pubkey::Pubkey;
use solana_sdk::{
    message::VersionedMessage,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::VersionedTransaction,
};

use crate::{
    intent_executor::{
        commit_id_fetcher::TaskInfoFetcher,
        error::{Error, IntentExecutorResult, InternalError},
        ExecutionOutput, IntentExecutor,
    },
    persist::{CommitStatus, CommitStatusSignatures, IntentPersister},
    tasks::{
        task_builder::{TaskBuilderV1, TasksBuilder},
        task_strategist::{TaskStrategist, TransactionStrategy},
        tasks::BaseTask,
    },
    transaction_preperator::transaction_preparator::TransactionPreparator,
    utils::persist_status_update_by_message_set,
};

pub struct IntentExecutorImpl<T, C> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
    task_info_fetcher: Arc<C>,
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

    fn try_unite_tasks<P: IntentPersister>(
        commit_tasks: &[Box<dyn BaseTask>],
        finalize_task: &[Box<dyn BaseTask>],
        authority: &Pubkey,
        persister: &Option<P>,
    ) -> Option<TransactionStrategy> {
        // Clone tasks since strategies applied to united case maybe suboptimal for regular one
        let mut commit_tasks = commit_tasks
            .iter()
            .map(|task| task.clone())
            .collect::<Vec<_>>();
        let finalize_task = finalize_task
            .iter()
            .map(|task| task.clone())
            .collect::<Vec<_>>();

        // Unite tasks to attempt running as single tx
        commit_tasks.extend(finalize_task);
        match TaskStrategist::build_strategy(commit_tasks, authority, persister)
        {
            Ok(strategy) => Some(strategy),
            Err(crate::tasks::task_strategist::Error::FailedToFitError) => None,
        }
    }

    async fn execute_inner<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        if base_intent.is_empty() {
            return Err(Error::EmptyIntentError);
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

        // Build tasks for Commit & Finalize stages
        let commit_tasks = TaskBuilderV1::commit_tasks(
            &self.task_info_fetcher,
            &base_intent,
            &persister,
        )
        .await?;
        let finalize_tasks = TaskBuilderV1::finalize_tasks(
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
        ) {
            info!("Executing intent in single stage");
            self.execute_single_stage(&single_tx_strategy, persister)
                .await
        } else {
            info!("Executing intent in two stages");
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

            self.execute_two_stages(
                &commit_strategy,
                &finalize_strategy,
                persister,
            )
            .await
        }
    }

    /// Optimization: executes Intent in single stage
    /// where Commit & Finalize are united
    // TODO: remove once challenge window introduced
    async fn execute_single_stage<P: IntentPersister>(
        &self,
        transaction_strategy: &TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let prepared_message = self
            .transaction_preparator
            .prepare_for_strategy(
                &self.authority,
                transaction_strategy,
                persister,
            )
            .await
            .map_err(Error::FailedFinalizePreparationError)?;

        let signature = self
            .send_prepared_message(prepared_message)
            .await
            .map_err(|(err, signature)| Error::FailedToCommitError {
                err,
                signature,
            })?;

        info!("Single stage intent executed: {}", signature);
        Ok(ExecutionOutput::SingleStage(signature))
    }

    /// Executes Intent in 2 stage: Commit & Finalize
    async fn execute_two_stages<P: IntentPersister>(
        &self,
        commit_strategy: &TransactionStrategy,
        finalize_strategy: &TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        // Prepare everything for Commit stage execution
        let prepared_commit_message = self
            .transaction_preparator
            .prepare_for_strategy(&self.authority, commit_strategy, persister)
            .await
            .map_err(Error::FailedCommitPreparationError)?;

        let commit_signature = self
            .send_prepared_message(prepared_commit_message)
            .await
            .map_err(|(err, signature)| Error::FailedToCommitError {
                err,
                signature,
            })?;
        info!("Commit stage succeeded: {}", commit_signature);

        // Prepare everything for Finalize stage execution
        let prepared_finalize_message = self
            .transaction_preparator
            .prepare_for_strategy(&self.authority, finalize_strategy, persister)
            .await
            .map_err(Error::FailedFinalizePreparationError)?;

        let finalize_signature = self
            .send_prepared_message(prepared_finalize_message)
            .await
            .map_err(|(err, finalize_signature)| {
                Error::FailedToFinalizeError {
                    err,
                    commit_signature: Some(commit_signature),
                    finalize_signature,
                }
            })?;
        info!("Finalize stage succeeded: {}", finalize_signature);

        Ok(ExecutionOutput::TwoStage {
            commit_signature,
            finalize_signature,
        })
    }

    /// Shared helper for sending transactions
    async fn send_prepared_message(
        &self,
        mut prepared_message: VersionedMessage,
    ) -> IntentExecutorResult<Signature, (InternalError, Option<Signature>)>
    {
        let latest_blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|err| (err.into(), None))?;
        match &mut prepared_message {
            VersionedMessage::V0(value) => {
                value.recent_blockhash = latest_blockhash;
            }
            VersionedMessage::Legacy(value) => {
                warn!("TransactionPreparator v1 does not use Legacy message");
                value.recent_blockhash = latest_blockhash;
            }
        };

        let transaction =
            VersionedTransaction::try_new(prepared_message, &[&self.authority])
                .map_err(|err| (err.into(), None))?;
        let result = self
            .rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await
            .map_err(|err| {
                let signature = err.signature();
                (err.into(), signature)
            })?;

        Ok(result.into_signature())
    }

    fn persist_result<P: IntentPersister>(
        persistor: &Option<P>,
        result: &IntentExecutorResult<ExecutionOutput>,
        message_id: u64,
        pubkeys: &[Pubkey],
    ) {
        match result {
            Ok(value) => {
                let signatures = match value {
                    ExecutionOutput::SingleStage(process_signature) => CommitStatusSignatures {
                        process_signature: *process_signature,
                        finalize_signature: None
                    },
                    ExecutionOutput::TwoStage {
                        commit_signature, finalize_signature
                    } => CommitStatusSignatures {
                        process_signature: *commit_signature,
                        finalize_signature: Some(*finalize_signature)
                    }
                };
                let update_status = CommitStatus::Succeeded(signatures);
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            },
            Err(Error::EmptyIntentError) | Err(Error::FailedToFitError) => {
                let update_status = CommitStatus::Failed;
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::TaskBuilderError(_)) => {
                let update_status = CommitStatus::Failed;
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::FailedCommitPreparationError(crate::transaction_preperator::error::Error::FailedToFitError)) => {
                let update_status = CommitStatus::PartOfTooLargeBundleToProcess;
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::FailedCommitPreparationError(crate::transaction_preperator::error::Error::DeliveryPreparationError(_))) => {
                // Persisted internally
            },
            Err(Error::FailedToCommitError {err: _, signature}) => {
                // Commit is a single TX, so if it fails, all of commited accounts marked FailedProcess
                let status_signature = signature.map(|sig| CommitStatusSignatures {
                    process_signature: sig,
                    finalize_signature: None
                });
                let update_status = CommitStatus::FailedProcess(status_signature);
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::FailedFinalizePreparationError(_)) => {
                // Not supported in persistor
            },
            Err(Error::FailedToFinalizeError {err: _, commit_signature, finalize_signature}) => {
                // Finalize is a single TX, so if it fails, all of commited accounts marked FailedFinalize
                let update_status = if let Some(commit_signature) = commit_signature {
                    let signatures = CommitStatusSignatures {
                        process_signature: *commit_signature,
                        finalize_signature: *finalize_signature
                    };
                    CommitStatus::FailedFinalize(signatures)
                } else {
                    CommitStatus::FailedProcess(None)
                };
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
        }
    }
}

#[async_trait::async_trait]
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
        let pubkeys = base_intent.get_committed_pubkeys();

        let result = self.execute_inner(base_intent, &persister).await;
        if let Some(pubkeys) = pubkeys {
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
            commit_id_fetcher::{TaskInfoFetcher, TaskInfoFetcherResult},
            IntentExecutorImpl,
        },
        persist::IntentPersisterImpl,
        tasks::task_builder::{TaskBuilderV1, TasksBuilder},
        transaction_preperator::transaction_preparator::TransactionPreparatorV1,
    };

    struct MockInfoFetcher;
    #[async_trait::async_trait]
    impl TaskInfoFetcher for MockInfoFetcher {
        fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<u64> {
            Some(0)
        }

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
    }

    #[tokio::test]
    async fn test_try_unite() {
        let pubkey = [Pubkey::new_unique()];
        let intent = create_test_intent(0, &pubkey);

        let info_fetcher = Arc::new(MockInfoFetcher);
        let commit_task = TaskBuilderV1::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();
        let finalize_task =
            TaskBuilderV1::finalize_tasks(&info_fetcher, &intent)
                .await
                .unwrap();

        let result = IntentExecutorImpl::<
            TransactionPreparatorV1,
            MockInfoFetcher,
        >::try_unite_tasks(
            &commit_task,
            &finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
        );
        assert!(result.is_some());

        let strategy = result.unwrap();
        assert!(strategy.lookup_tables_keys.is_empty());
    }
}
