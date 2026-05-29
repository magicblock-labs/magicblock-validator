use std::sync::Arc;

use async_trait::async_trait;
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::{
    link::transactions::{with_encoded, TransactionSchedulerHandle},
    traits::LatestBlockProvider,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    register_scheduled_commit_sent, MagicContext, SentCommit,
    TransactionScheduler, MAGIC_CONTEXT_PUBKEY,
};
use solana_account::ReadableAccount;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use tracing::{debug, error};

#[async_trait]
pub trait IntentRpcClient: Send + Sync + 'static {
    type Error: std::error::Error + Send;

    /// Executes `Accept` tx and returns accepted intents
    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, Self::Error>;

    /// Processes intent results, submitting them on chain(ER)
    async fn finalize_intent(
        &self,
        sent_tx: Transaction,
        sent_commit: SentCommit,
    ) -> Result<(), Self::Error>;
}

pub struct InternalIntentRpcClient<L: LatestBlockProvider> {
    /// Provides access to MagicContext
    accounts_db: Arc<AccountsDb>,
    /// Internal endpoint for scheduling ER TXs
    transaction_scheduler: TransactionSchedulerHandle,
    /// Provides access to ER latest block for TX creation
    latest_block_provider: L,
}

impl<L: LatestBlockProvider> InternalIntentRpcClient<L> {
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
    async fn execute_accept_tx(&self) -> Result<(), InternalRpcClientError> {
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
impl<L: LatestBlockProvider> IntentRpcClient for InternalIntentRpcClient<L> {
    type Error = InternalRpcClientError;

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
        self.execute_accept_tx().await?;

        // Return intents from global store
        Ok(TransactionScheduler::default().take_scheduled_intent_bundles())
    }

    async fn finalize_intent(
        &self,
        sent_tx: Transaction,
        sent_commit: SentCommit,
    ) -> Result<(), Self::Error> {
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
}

#[derive(thiserror::Error, Debug)]
pub enum InternalRpcClientError {
    #[error("TransactionError: {0}")]
    TransactionError(#[from] TransactionError),
}
