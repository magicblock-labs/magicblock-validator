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
    signature::Keypair,
    signer::{Signer, SignerError},
    transaction::VersionedTransaction,
};

use crate::{
    persist::L1MessagesPersisterIface,
    transaction_preperator::transaction_preparator::{
        TransactionPreparator, TransactionPreparatorV1,
    },
    ComputeBudgetConfig,
};

// TODO(edwin): define struct
// (commit_id, signature)s that it sent. Single worker in [`RemoteScheduledCommitsProcessor`]
#[derive(Clone, Debug)]
pub struct ExecutionOutput {}

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
        // Commit message first
        self.commit(&l1_message, commit_ids).await?;
        // At the moment validator finalizes right away
        // In the future there will be a challenge window
        self.finalize(&l1_message).await?;
        Ok(ExecutionOutput {})
    }

    /// Executes Commit stage
    async fn commit<P: L1MessagesPersisterIface>(
        &self,
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
        persister: Option<P>,
    ) -> MessageExecutorResult<()> {
        let prepared_message = self
            .transaction_preparator
            .prepare_commit_tx(
                &self.authority,
                l1_message,
                commit_ids,
                &persister,
            )
            .await?;

        self.send_prepared_message(prepared_message).await
    }

    /// Executes Finalize stage
    async fn finalize<P: L1MessagesPersisterIface>(
        &self,
        l1_message: &ScheduledL1Message,
        persister: Option<P>,
    ) -> MessageExecutorResult<()> {
        let rent_reimbursement = self.authority.pubkey();
        let prepared_message = self
            .transaction_preparator
            .prepare_finalize_tx(
                &self.authority,
                &rent_reimbursement,
                l1_message,
                &persister,
            )
            .await?;

        self.send_prepared_message(prepared_message).await
    }

    /// Shared helper for sending transactions
    async fn send_prepared_message(
        &self,
        mut prepared_message: VersionedMessage,
    ) -> MessageExecutorResult<()> {
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
        self.rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;

        Ok(())
    }
}

// TODO(edwin): properly define
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("SignerError: {0}")]
    SignerError(#[from] SignerError),
    #[error("PreparatorError: {0}")]
    PreparatorError(#[from] crate::transaction_preperator::error::Error),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
}

pub type MessageExecutorResult<T, E = Error> = Result<T, E>;
