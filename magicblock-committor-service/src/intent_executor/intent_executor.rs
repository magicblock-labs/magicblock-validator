use log::{debug, warn};
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
    transaction::VersionedTransaction,
};

use crate::{
    intent_executor::{
        error::{Error, IntentExecutorResult, InternalError},
        ExecutionOutput, IntentExecutor,
    },
    persist::{CommitStatus, CommitStatusSignatures, IntentPersister},
    transaction_preparator::transaction_preparator::TransactionPreparator,
    utils::persist_status_update_by_message_set,
};

pub struct IntentExecutorImpl<T> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
}

impl<T> IntentExecutorImpl<T>
where
    T: TransactionPreparator,
{
    pub fn new(
        rpc_client: MagicblockRpcClient,
        transaction_preparator: T,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            rpc_client,
            transaction_preparator,
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

        // Commit stage
        let commit_signature =
            self.execute_commit_stage(&base_intent, persister).await?;
        debug!("Commit stage succeeded: {}", commit_signature);

        // Finalize stage
        // At the moment validator finalizes right away
        // In the future there will be a challenge window
        let finalize_signature = self
            .execute_finalize_stage(&base_intent, commit_signature, persister)
            .await?;
        debug!("Finalize stage succeeded: {}", finalize_signature);

        Ok(ExecutionOutput {
            commit_signature,
            finalize_signature,
        })
    }

    async fn execute_commit_stage<P: IntentPersister>(
        &self,
        l1_message: &ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        let prepared_message = self
            .transaction_preparator
            .prepare_commit_tx(&self.authority, l1_message, persister)
            .await
            .map_err(Error::FailedCommitPreparationError)?;

        self.send_prepared_message(prepared_message).await.map_err(
            |(err, signature)| Error::FailedToCommitError { err, signature },
        )
    }

    async fn execute_finalize_stage<P: IntentPersister>(
        &self,
        l1_message: &ScheduledBaseIntent,
        commit_signature: Signature,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature> {
        let prepared_message = self
            .transaction_preparator
            .prepare_finalize_tx(&self.authority, l1_message, persister)
            .await
            .map_err(Error::FailedFinalizePreparationError)?;

        self.send_prepared_message(prepared_message).await.map_err(
            |(err, finalize_signature)| Error::FailedToFinalizeError {
                err,
                commit_signature,
                finalize_signature,
            },
        )
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
                let signatures = CommitStatusSignatures {
                    process_signature: value.commit_signature,
                    finalize_signature: Some(value.commit_signature)
                };
                let update_status = CommitStatus::Succeeded(signatures);
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::EmptyIntentError) => {
                let update_status = CommitStatus::Failed;
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::FailedCommitPreparationError(crate::transaction_preparator::error::Error::FailedToFitError)) => {
                let update_status = CommitStatus::PartOfTooLargeBundleToProcess;
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::FailedCommitPreparationError(crate::transaction_preparator::error::Error::TaskBuilderError(err))) => {
                match err {
                    crate::tasks::task_builder::Error::CommitTasksBuildError(_) => {
                        let update_status = CommitStatus::Failed;
                        persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
                    }
                    crate::tasks::task_builder::Error::FinalizedTasksBuildError(_) => {
                        // During commit preparation we don't encounter following error
                        // so no need to persist it
                    }
                 }
            },
            Err(Error::FailedCommitPreparationError(crate::transaction_preparator::error::Error::DeliveryPreparationError(_))) => {
                // Intermediate commit preparation progress recorded by DeliveryPreparator
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
                let status_signature = CommitStatusSignatures {
                    process_signature: *commit_signature,
                    finalize_signature: *finalize_signature
                };
                let update_status = CommitStatus::FailedFinalize( status_signature);
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
        }
    }
}

#[async_trait::async_trait]
impl<T> IntentExecutor for IntentExecutorImpl<T>
where
    T: TransactionPreparator,
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
