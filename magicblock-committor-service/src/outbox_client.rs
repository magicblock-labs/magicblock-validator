use std::sync::Arc;

use async_trait::async_trait;
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::{
    link::transactions::{with_encoded, TransactionSchedulerHandle},
    traits::LatestBlockProvider,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
    register_scheduled_commit_sent, MagicContext, SentCommit,
    TransactionScheduler, MAGIC_CONTEXT_PUBKEY,
};
use solana_account::ReadableAccount;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use tracing::{debug, error};

use crate::service::outbox_intent_bundles_reader::{
    InternalOutboxIntentBundlesReader, OutboxIntentBundlesReader,
};

#[async_trait]
pub trait OutboxClient: Send + Sync + 'static {
    type Error: std::error::Error + Send;
    /// Type that is able to read IntentBundles from Outbox
    /// Can be via AccountsDB, RpcClient or any other means
    type OutboxReader: OutboxIntentBundlesReader;

    /// Executes `Accept` tx and returns accepted intents
    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, Self::Error>;

    /// Sets execution stage for outbox intent
    /// Note: intent has to be accepted prior
    /// Calling with invalid state transitions will lead to `TransactionError`
    async fn set_intent_execution_stage(
        &self,
        intent_id: u64,
        stage: ExecutionStage,
    ) -> Result<(), Self::Error>;

    /// Processes intent results, submitting them on chain(ER)
    async fn notify_commit_sent(
        &self,
        sent_tx: Transaction,
        sent_commit: SentCommit,
    ) -> Result<(), Self::Error>;

    /// Returns reader capable of reading IntentBundles from Outbox
    fn outbox_reader(&self) -> Self::OutboxReader;
}

/// Implementation of `OutboxClient` that uses ER internals
/// Potentially could be replaced with RPC base Client
pub struct InternalOutboxClient<L: LatestBlockProvider> {
    /// Provides access to MagicContext
    accounts_db: Arc<AccountsDb>,
    /// Internal endpoint for scheduling ER TXs
    transaction_scheduler: TransactionSchedulerHandle,
    /// Provides access to ER latest block for TX creation
    latest_block_provider: L,
}

impl<L: LatestBlockProvider> InternalOutboxClient<L> {
    pub fn new(
        accounts_db: Arc<AccountsDb>,
        transaction_scheduler: TransactionSchedulerHandle,
        latest_block_provider: L,
    ) -> Self {
        Self {
            accounts_db,
            transaction_scheduler,
            latest_block_provider,
        }
    }

    /// Sends transaction to move the scheduled commits from the `MagicContext`
    /// to the global ScheduledCommit store
    async fn send_accept_tx(&self) -> Result<(), InternalOutboxClientError> {
        let tx = InstructionUtils::accept_scheduled_commits(
            self.latest_block_provider.blockhash(),
        );
        let encoded_tx = with_encoded(tx).inspect_err(|err| {
            error!(error = ?err, "Failed to bincode intent transaction");
        })?;
        self.transaction_scheduler
            .execute(encoded_tx)
            .await
            .inspect_err(|err| {
                error!(error = ?err, "Failed to accept scheduled commits");
            })?;

        Ok(())
    }
}

#[async_trait]
impl<L: LatestBlockProvider> OutboxClient for InternalOutboxClient<L> {
    type Error = InternalOutboxClientError;
    type OutboxReader = InternalOutboxIntentBundlesReader;

    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, Self::Error> {
        // If accounts were scheduled to be committed, we accept them here
        // and processs the commits
        let magic_context_acc =
            self.accounts_db.get_account(&MAGIC_CONTEXT_PUBKEY).expect(
                "Validator found to be running without MagicContext account!",
            );
        if !MagicContext::has_scheduled_commits(magic_context_acc.data()) {
            return Ok(vec![]);
        }
        self.send_accept_tx().await?;

        // Return intents from global store
        Ok(TransactionScheduler::default().take_scheduled_intent_bundles())
    }

    async fn set_intent_execution_stage(
        &self,
        intent_id: u64,
        stage: ExecutionStage,
    ) -> Result<(), Self::Error> {
        // TODO(edwin): use rpc of scheduler
        todo!()
    }

    async fn notify_commit_sent(
        &self,
        sent_tx: Transaction,
        sent_commit: SentCommit,
    ) -> Result<(), Self::Error> {
        // TODO(edwin): is using handle directly here ok? This could require Chainlink mechanics
        register_scheduled_commit_sent(sent_commit);
        let txn = with_encoded(sent_tx).inspect_err(|err| {
            // Unreachable case, all intent transactions are smaller than 64KB by construction
            error!(error = ?err, "Failed to bincode intent transaction");
        })?;
        self.transaction_scheduler
            .execute(txn)
            .await
            .inspect(|_| debug!("Sent commit signaled"))
            .inspect_err(
                |err| error!(error = ?err, "Failed to signal sent commit"),
            )?;

        Ok(())
    }

    fn outbox_reader(&self) -> Self::OutboxReader {
        InternalOutboxIntentBundlesReader::new(self.accounts_db.clone())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalOutboxClientError {
    #[error("TransactionError: {0}")]
    TransactionError(#[from] TransactionError),
}
