use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::{Arc, Mutex},
};

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
    commit_scheduler::commit_scheduler_inner::{
        CommitSchedulerInner, POISONED_INNER_MSG,
    },
    transaction_preperator::transaction_preparator::{
        TransactionPreparator, TransactionPreparatorV1,
    },
    ComputeBudgetConfig,
};

pub(crate) struct L1MessageExecutor<T: TransactionPreparator> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
    inner: Arc<Mutex<CommitSchedulerInner>>,
}

impl<T: TransactionPreparator> L1MessageExecutor<T> {
    pub fn new_v1(
        inner: Arc<Mutex<CommitSchedulerInner>>,
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
        Self {
            authority,
            rpc_client,
            transaction_preparator,
            inner,
        }
    }

    /// Executes message on L1
    pub async fn execute(
        mut self,
        l1_message: ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> MessageExecutorResult<()> {
        // Commit message first
        self.commit(&l1_message, commit_ids).await?;
        // At the moment validator finalizes right away
        // In the future there will be a challenge window
        self.finalize(&l1_message).await?;
        // Signal that task is completed
        // TODO(edwin): handle error case here as well
        self.inner
            .lock()
            .expect(POISONED_INNER_MSG)
            .complete(&l1_message);
        Ok(())
    }

    /// Executes Commit stage
    async fn commit(
        &self,
        l1_message: &ScheduledL1Message,
        commit_ids: HashMap<Pubkey, u64>,
    ) -> MessageExecutorResult<()> {
        let mut prepared_message = self
            .transaction_preparator
            .prepare_commit_tx(&self.authority, &l1_message, commit_ids)
            .await?;

        let latest_blockhash = self.rpc_client.get_latest_blockhash()?;
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
        // TODO(edwin): add retries here?
        self.rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;
        Ok(())
    }

    /// Executes Finalize stage
    async fn finalize(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> MessageExecutorResult<()> {
        // TODO(edwin): properly define this.
        let rent_reimbursement = self.authority.pubkey();
        let mut prepared_message = self
            .transaction_preparator
            .prepare_finalize_tx(
                &self.authority,
                &rent_reimbursement,
                &l1_message,
            )
            .await?;

        let latest_blockhash = self.rpc_client.get_latest_blockhash()?;
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
        // TODO(edwin): add retries here?
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
