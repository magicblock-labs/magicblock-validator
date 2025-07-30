use std::{collections::HashMap, sync::Arc};

use log::warn;
use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message,
    validator::validator_authority,
};
use magicblock_rpc_client::{
    MagicBlockSendTransactionConfig, MagicblockRpcClient,
};
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
use solana_sdk::{
    message::VersionedMessage,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::VersionedTransaction,
};

use crate::{
    commit_scheduler::commit_id_tracker::CommitIdFetcher,
    message_executor::{
        error::{Error, InternalError, MessageExecutorResult},
        ExecutionOutput, MessageExecutor,
    },
    persist::{CommitStatus, CommitStatusSignatures, L1MessagesPersisterIface},
    transaction_preperator::transaction_preparator::{
        TransactionPreparator, TransactionPreparatorV1,
    },
    utils::{
        persist_status_update, persist_status_update_by_message_set,
        persist_status_update_set,
    },
    ComputeBudgetConfig,
};

pub struct L1MessageExecutor<T> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
}

impl<T> L1MessageExecutor<T>
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

    async fn execute_inner<P: L1MessagesPersisterIface>(
        &self,
        l1_message: ScheduledL1Message,
        persister: &Option<P>,
    ) -> MessageExecutorResult<ExecutionOutput> {
        // Update tasks status to Pending
        // let update_status = CommitStatus::Pending;
        // persist_status_update_set(&persister, &commit_ids, update_status);

        // Commit stage
        let commit_signature =
            self.execute_commit_stage(&l1_message, persister).await?;

        // Finalize stage
        // At the moment validator finalizes right away
        // In the future there will be a challenge window
        let finalize_signature = self
            .execute_finalize_stage(&l1_message, commit_signature, persister)
            .await?;

        Ok(ExecutionOutput {
            commit_signature,
            finalize_signature,
        })
    }

    async fn execute_commit_stage<P: L1MessagesPersisterIface>(
        &self,
        l1_message: &ScheduledL1Message,
        persister: &Option<P>,
    ) -> MessageExecutorResult<Signature> {
        let prepared_message = self
            .transaction_preparator
            .prepare_commit_tx(&self.authority, l1_message, persister)
            .await
            .map_err(Error::FailedCommitPreparationError)?;

        self.send_prepared_message(prepared_message).await.map_err(
            |(err, signature)| Error::FailedToCommitError { err, signature },
        )
    }

    async fn execute_finalize_stage<P: L1MessagesPersisterIface>(
        &self,
        l1_message: &ScheduledL1Message,
        commit_signature: Signature,
        persister: &Option<P>,
    ) -> MessageExecutorResult<Signature> {
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
    ) -> MessageExecutorResult<Signature, (InternalError, Option<Signature>)>
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

    fn persist_result<P: L1MessagesPersisterIface>(
        persistor: &Option<P>,
        result: &MessageExecutorResult<ExecutionOutput>,
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
            Err(Error::FailedCommitPreparationError(crate::transaction_preperator::error::Error::FailedToFitError)) => {
                let update_status = CommitStatus::PartOfTooLargeBundleToProcess;
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            }
            Err(Error::FailedCommitPreparationError(crate::transaction_preperator::error::Error::TaskBuilderError(_))) => {
                let update_status = CommitStatus::Failed;
                persist_status_update_by_message_set(persistor, message_id, pubkeys, update_status);
            },
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
impl<T> MessageExecutor for L1MessageExecutor<T>
where
    T: TransactionPreparator,
{
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: L1MessagesPersisterIface>(
        &self,
        l1_message: ScheduledL1Message,
        persister: Option<P>,
    ) -> MessageExecutorResult<ExecutionOutput> {
        let message_id = l1_message.id;
        let pubkeys = l1_message.get_committed_pubkeys();

        let result = self.execute_inner(l1_message, &persister).await;
        if let Some(pubkeys) = pubkeys {
            Self::persist_result(&persister, &result, message_id, &pubkeys);
        }

        result
    }
}
