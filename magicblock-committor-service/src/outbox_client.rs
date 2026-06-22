use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use async_trait::async_trait;
use backoff::{future::retry, ExponentialBackoff};
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::{
    link::{
        accounts::LockedAccount,
        transactions::{with_encoded, TransactionSchedulerHandle},
    },
    traits::LatestBlockProvider,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
    register_scheduled_commit_sent, MagicContext, Pubkey, SentCommit,
    MAGIC_CONTEXT_PUBKEY,
};
use solana_account::{Account, ReadableAccount};
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::{
    client_error, client_error::ErrorKind as RpcClientErrorKind,
};
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
    ) -> Result<
        Vec<ScheduledIntentBundle>,
        (Vec<ScheduledIntentBundle>, Self::Error),
    >;

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
    /// RPC client for sending accept transactions to the ER
    // TODO(edwin): check if needs to be Arc
    rpc_client: Arc<RpcClient>,
    /// Internal endpoint for scheduling ER TXs
    transaction_scheduler: TransactionSchedulerHandle,
    /// Provides access to ER latest block for TX creation
    latest_block_provider: L,
}

impl<L: LatestBlockProvider> InternalOutboxClient<L> {
    pub fn new(
        accounts_db: Arc<AccountsDb>,
        rpc_client: Arc<RpcClient>,
        transaction_scheduler: TransactionSchedulerHandle,
        latest_block_provider: L,
    ) -> Self {
        Self {
            accounts_db,
            rpc_client,
            transaction_scheduler,
            latest_block_provider,
        }
    }

    async fn send_with_backoff(
        &self,
        backoff_config: ExponentialBackoff,
        tx: &impl SerializableTransaction,
    ) -> Result<(), client_error::Error> {
        let signature = tx.get_signature();
        retry(backoff_config, || async {
            self.rpc_client
                .send_and_confirm_transaction(tx)
                .await
                .map_err(|err| {
                    match err.kind() {
                        RpcClientErrorKind::TransactionError(_) => {
                            backoff::Error::Permanent(err)
                        }
                        _ => {
                            error!(signature = ?signature, error = ?err, "Transient error accepting intents, retrying");
                            backoff::Error::transient(err)
                        }
                    }
                })
        }).await?;

        Ok(())
    }

    /// Sends `AcceptScheduledCommits` transactions to the ER, moving scheduled
    /// commits from `MagicContext` into outbox PDA accounts, up to CHUNK_SIZE intents per transaction.
    /// On first error returns the successfully accepted intents so far alongside the error.
    async fn send_accept_tx(
        &self,
        scheduled_intents: Vec<ScheduledIntentBundle>,
    ) -> Result<
        Vec<ScheduledIntentBundle>,
        (Vec<ScheduledIntentBundle>, InternalOutboxClientError),
    > {
        const CHUNK_SIZE: usize = 50;

        let mut remaining = scheduled_intents;
        let mut accepted = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let chunk_size = CHUNK_SIZE.min(remaining.len());
            let tx = InstructionUtils::accept_scheduled_commits(
                self.latest_block_provider.blockhash(),
                remaining[..chunk_size].iter().map(|i| i.id),
            );
            let backoff_config = ExponentialBackoff {
                max_elapsed_time: Some(Duration::from_secs(25)),
                max_interval: Duration::from_secs(5),
                ..ExponentialBackoff::default()
            };
            match self.send_with_backoff(backoff_config, &tx).await {
                Ok(_) => accepted.extend(remaining.drain(..chunk_size)),
                Err(err) => return Err((accepted, err.into())),
            }
        }

        Ok(accepted)
    }

    /// Returns account corresponding to `pubkey` if it exists
    /// Safely handles race-conditions and any concurrent changes to an account
    fn safe_get_account(&self, pubkey: &Pubkey) -> Option<Account> {
        let shared_account = self.accounts_db.get_account(pubkey)?;
        let locked_account = LockedAccount::new(*pubkey, shared_account);
        let output = locked_account
            .read_locked(|_, account| Account::from(account.clone()));

        Some(output)
    }
}

#[async_trait]
impl<L: LatestBlockProvider> OutboxClient for InternalOutboxClient<L> {
    type Error = InternalOutboxClientError;
    type OutboxReader = InternalOutboxIntentBundlesReader;

    async fn accept_scheduled_intents(
        &self,
    ) -> Result<
        Vec<ScheduledIntentBundle>,
        (Vec<ScheduledIntentBundle>, Self::Error),
    > {
        // If accounts were scheduled to be committed, we accept them here
        // and processs the commits
        let magic_context_acc =
            self.safe_get_account(&MAGIC_CONTEXT_PUBKEY).expect(
                "Validator found to be running without MagicContext account!",
            );

        let magic_context = MagicContext::deserialize(magic_context_acc.data())
            .map_err(|err| (vec![], err.into()))?;
        self.send_accept_tx(magic_context.scheduled_base_intents)
            .await
    }

    async fn set_intent_execution_stage(
        &self,
        intent_id: u64,
        stage: ExecutionStage,
    ) -> Result<(), Self::Error> {
        let tx = InstructionUtils::set_intent_execution_stage(
            self.latest_block_provider.blockhash(),
            intent_id,
            stage,
        );

        self.send_with_backoff(
            ExponentialBackoff {
                max_elapsed_time: Some(Duration::from_secs(25)),
                max_interval: Duration::from_secs(5),
                ..ExponentialBackoff::default()
            },
            &tx,
        )
        .await
        .map_err(Into::into)
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
        const CAPACITY: NonZeroUsize = NonZeroUsize::new(1000).unwrap();
        InternalOutboxIntentBundlesReader::new(
            self.accounts_db.clone(),
            CAPACITY,
        )
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalOutboxClientError {
    #[error("TransactionError: {0}")]
    TransactionError(#[from] TransactionError),
    #[error("RpcClientError: {0}")]
    RpcClientError(#[from] client_error::Error),
    #[error("BincodeError: {0}")]
    BincodeError(#[from] bincode::Error),
}

pub type InternalOutboxClientResult<T> = Result<T, InternalOutboxClientError>;
