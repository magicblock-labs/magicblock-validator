use std::collections::HashMap;

use log::warn;
use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message,
    validator::validator_authority,
};
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicblockRpcClient,
};
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
use solana_sdk::{
    message::VersionedMessage,
    signature::{Keypair, Signature},
    signer::{Signer, SignerError},
    transaction::VersionedTransaction,
};

use crate::{
    persist::{CommitStatus, L1MessagesPersisterIface},
    transaction_preperator::transaction_preparator::{
        TransactionPreparator, TransactionPreparatorV1,
    },
    utils::persist_status_update_set,
    ComputeBudgetConfig,
};

// TODO(edwin): define struct
// (commit_id, signature)s that it sent. Single worker in [`RemoteScheduledCommitsProcessor`]
#[derive(Clone, Debug)]
pub struct ExecutionOutput {
    commit_signature: Signature,
    finalize_signature: Signature,
}

pub(crate) struct L1MessageExecutor<T> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
}

impl<T> L1MessageExecutor<T>
where
    T: TransactionPreparator,
{
    pub fn new_v1(
        rpc_client: MagicblockRpcClient,
        table_mania: TableMania,
        compute_budget_config: ComputeBudgetConfig,
    ) -> L1MessageExecutor<TransactionPreparatorV1> {
        let authority = validator_authority();
        let transaction_preparator = TransactionPreparatorV1::new(
            rpc_client.clone(),
            table_mania,
            compute_budget_config,
        );
        L1MessageExecutor::<TransactionPreparatorV1> {
            authority,
            rpc_client,
            transaction_preparator,
        }
    }

    /// Executes message on L1
    pub async fn execute<P: L1MessagesPersisterIface>(
        &self,
        l1_message: ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
        persister: Option<P>,
    ) -> MessageExecutorResult<ExecutionOutput> {
        // Update tasks status to Pending
        {
            let update_status = CommitStatus::Pending;
            persist_status_update_set(&persister, &commit_ids, update_status);
        }

        // Commit stage
        let commit_signature = {
            // Prepare everything for commit
            let prepared_message = self
                .transaction_preparator
                .prepare_commit_tx(
                    &self.authority,
                    &l1_message,
                    commit_ids,
                    &persister,
                )
                .await?;

            // Commit
            self.send_prepared_message(prepared_message).await.map_err(
                |(err, signature)| Error::FailedToCommitError {
                    err,
                    signature,
                },
            )?
        };

        // Finalize stage
        // At the moment validator finalizes right away
        // In the future there will be a challenge window
        let finalize_signature = {
            // Prepare eveything for finalize
            let rent_reimbursement = self.authority.pubkey();
            let prepared_message = self
                .transaction_preparator
                .prepare_finalize_tx(
                    &self.authority,
                    &rent_reimbursement,
                    &l1_message,
                    &persister,
                )
                .await?;

            // Finalize
            self.send_prepared_message(prepared_message).await.map_err(
                |(err, finalize_signature)| Error::FailedToFinalizeError {
                    err,
                    commit_signature,
                    finalize_signature,
                },
            )?
        };

        Ok(ExecutionOutput {
            commit_signature,
            finalize_signature,
        })
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
}

#[derive(thiserror::Error, Debug)]
pub enum InternalError {
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("FailedToCommitError: {0}")]
    FailedToCommitError {
        #[source]
        err: InternalError,
        signature: Option<Signature>,
    },
    #[error("FailedToFinalizeError: {0}")]
    FailedToFinalizeError {
        #[source]
        err: InternalError,
        commit_signature: Signature,
        finalize_signature: Option<Signature>,
    },
    #[error("PreparatorError: {0}")]
    FailedToPrepareTransactionError(
        #[from] crate::transaction_preperator::error::Error,
    ),
}

pub type MessageExecutorResult<T, E = Error> = Result<T, E>;
