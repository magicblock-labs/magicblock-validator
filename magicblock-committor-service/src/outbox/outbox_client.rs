use std::{mem, num::NonZeroUsize, sync::Arc, time::Duration};

use accountsdb::AccountsDBError;
use async_trait::async_trait;
use backoff::{ExponentialBackoff, future::retry};
use engine::{Engine, EngineError, IntoTransactionView};
use magicblock_program::{
    MAGIC_CONTEXT_PUBKEY, MagicContext, instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
    register_scheduled_commit_sent,
};
use solana_account::ReadableAccount;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::{
    client_error, client_error::ErrorKind as RpcClientErrorKind,
};
use solana_transaction_error::TransactionError;
use tracing::{debug, error};

use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport, error::IntentExecutorResult,
    },
    outbox::{
        IntentSentTransaction, OutboxClient, ScheduledBaseIntentMeta,
        outbox_intent_bundles_reader::InternalOutboxIntentBundlesReader,
        utils::build_sent_commit,
    },
};

/// Implementation of `OutboxClient` that uses ER internals.
///
/// Accept/set-stage/close still go through the RPC client (aperture) with
/// backoff retries; `notify_commit_sent` submits directly through the engine.
pub struct InternalOutboxClient {
    /// Engine handle: reads MagicContext, provides blockhash, and submits the
    /// `ScheduledCommitSent` notification directly.
    engine: Engine,
    /// RPC client for sending accept/set-stage/close transactions to the ER
    rpc_client: Arc<RpcClient>,
}

impl InternalOutboxClient {
    pub fn new(engine: Engine, rpc_client: Arc<RpcClient>) -> Self {
        Self { engine, rpc_client }
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

    /// Submits `transaction` directly through the engine, signed with the
    /// engine's authority.
    async fn execute_via_engine<T: IntoTransactionView>(
        &self,
        transaction: T,
    ) -> Result<(), InternalOutboxClientError> {
        self.engine.transaction(transaction)?.execute().await??;
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
                self.engine.blockhash(),
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
}

#[async_trait]
impl OutboxClient for InternalOutboxClient {
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
        let magic_context = self
            .engine
            .accounts()
            .loader()
            .read(&MAGIC_CONTEXT_PUBKEY, |account| {
                MagicContext::deserialize(account.data())
            })
            .map_err(|err| (vec![], err.into()))?
            .expect(
                "Validator found to be running without MagicContext account!",
            )
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
            self.engine.blockhash(),
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

    async fn close_intent(&self, intent_id: u64) -> Result<(), Self::Error> {
        let tx = InstructionUtils::close_outbox_intent(
            intent_id,
            self.engine.blockhash(),
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
        mut meta: ScheduledBaseIntentMeta,
        result: &IntentExecutorResult<ExecutionOutput>,
        execution_report: &IntentExecutionReport,
    ) -> Result<(), Self::Error> {
        let tx = match mem::take(&mut meta.intent_sent_transaction) {
            IntentSentTransaction::Known(tx) => tx,
            IntentSentTransaction::Recovered => {
                let blockhash = self.engine.blockhash();
                InstructionUtils::scheduled_commit_sent(meta.id, blockhash)
            }
        };
        let sent_commit = build_sent_commit(meta, result, execution_report);
        // TODO(edwin): is using handle directly here ok? This could require Chainlink mechanics
        register_scheduled_commit_sent(sent_commit);
        self.execute_via_engine(tx)
            .await
            .inspect(|_| debug!("Sent commit signaled"))
            .inspect_err(
                |err| error!(error = ?err, "Failed to signal sent commit"),
            )?;

        Ok(())
    }

    fn outbox_reader(&self) -> Self::OutboxReader {
        const CAPACITY: NonZeroUsize = NonZeroUsize::new(1000).unwrap();
        InternalOutboxIntentBundlesReader::new(self.engine.clone(), CAPACITY)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalOutboxClientError {
    #[error("TransactionError: {0}")]
    TransactionError(#[from] TransactionError),
    #[error("RpcClientError: {0}")]
    RpcClientError(#[from] client_error::Error),
    #[error("EngineError: {0}")]
    EngineError(#[from] EngineError),
    #[error("WincodeError: {0}")]
    WincodeError(#[from] wincode::ReadError),
    #[error("AccountsDbError: {0}")]
    AccountsDbError(#[from] AccountsDBError),
}

pub type InternalOutboxClientResult<T> = Result<T, InternalOutboxClientError>;
