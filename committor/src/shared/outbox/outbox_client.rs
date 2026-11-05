use std::{num::NonZeroUsize, time::Duration};

use accountsdb::AccountsDBError;
use async_trait::async_trait;
use engine::{Engine, EngineError, IntoTransactionView};
use mbv_core::intent::outbox::outbox_intent_pda;
use program::{
    MAGIC_CONTEXT_PUBKEY, MagicContext, SentCommit, instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle, outbox::ExecutionStage,
};
use solana_account::{AccountBuilder, AccountMode, ReadableAccount};
use solana_instruction::Instruction;
use solana_keypair::Address as Pubkey;
use solana_sdk_ids::system_program;
use solana_signature::Signature;
use solana_transaction_error::TransactionError;
use tokio::time::{Instant, sleep};
use tracing::{debug, error};

use crate::shared::outbox::{
    OutboxClient,
    outbox_intent_bundles_reader::{InternalOutboxIntentBundlesReader, fetch_outbox_intent},
};

const ALREADY_PROCESSED_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(200);
const ALREADY_PROCESSED_STATUS_TIMEOUT: Duration = Duration::from_secs(25);

/// Validator-local outbox bookkeeping submitted directly through Engine.
pub(crate) struct InternalOutboxClient {
    engine: Engine,
}

impl InternalOutboxClient {
    pub(crate) fn new(engine: Engine) -> Self {
        Self { engine }
    }

    async fn wait_for_engine_signature_success(
        &self,
        signature: Signature,
    ) -> Result<(), InternalOutboxClientError> {
        let start = Instant::now();
        loop {
            if let Some(status) =
                self.engine.transactions().status(signature).await.map_err(EngineError::from)?
            {
                return status.result.map_err(Into::into);
            }

            if start.elapsed() >= ALREADY_PROCESSED_STATUS_TIMEOUT {
                return Err(InternalOutboxClientError::TransactionStatusMissing(
                    signature,
                ));
            }

            sleep(ALREADY_PROCESSED_STATUS_POLL_INTERVAL).await;
        }
    }

    /// Submits local bookkeeping instructions signed with Engine's authority.
    async fn execute(&self, instructions: &[Instruction]) -> Result<(), InternalOutboxClientError> {
        let transaction = instructions.compose(&self.engine)?;
        let signature = transaction.signatures()[0];
        match self.engine.transaction(transaction)?.execute().await? {
            Ok(()) => Ok(()),
            Err(TransactionError::AlreadyProcessed) => {
                self.wait_for_engine_signature_success(signature).await
            }
            Err(err) => Err(err.into()),
        }
    }

    // Only a base mirror may be replaced; require its exact image because
    // ordinary transactions are not serialized by the materialization lease.
    async fn prepare_outbox(&self, intent_id: u64) -> Result<(), InternalOutboxClientError> {
        let pubkey = outbox_intent_pda(intent_id);
        let accessor = self.engine.account(pubkey).await?;
        if !matches!(accessor.observed(), Some((AccountMode::Mirrored, _))) {
            return Ok(());
        }
        let Some(expected) =
            self.engine.accounts().loader().read(&pubkey, |account| account.owned())?
        else {
            return Ok(());
        };
        if !expected.is(AccountMode::Mirrored)
            || expected.owner() != system_program::ID
            || !expected.data().is_empty()
        {
            return Err(InternalOutboxClientError::InvalidOutboxTarget(pubkey));
        }
        let account = AccountBuilder::default().slot(expected.slot()).mode(AccountMode::Magic);
        accessor.require(&expected).materialize(account, None).await?;
        Ok(())
    }

    async fn accept_chunk(
        &self,
        intents: &[ScheduledIntentBundle],
    ) -> Result<(), InternalOutboxClientError> {
        for intent in intents {
            self.prepare_outbox(intent.intent_id).await?;
        }
        let ix = InstructionUtils::accept_scheduled_commits_instruction(
            self.engine.authority(),
            intents.iter().map(|intent| intent.intent_id),
        );
        self.execute(&[ix]).await
    }

    /// Sends `AcceptScheduledCommits` transactions to the ER, moving scheduled
    /// commits from `MagicContext` into outbox PDA accounts, up to CHUNK_SIZE intents per transaction.
    /// On first error returns the successfully accepted intents so far alongside the error.
    async fn send_accept_tx(
        &self,
        scheduled_intents: Vec<ScheduledIntentBundle>,
    ) -> Result<Vec<ScheduledIntentBundle>, (Vec<ScheduledIntentBundle>, InternalOutboxClientError)>
    {
        const CHUNK_SIZE: usize = 50;

        let mut accepted = Vec::with_capacity(scheduled_intents.len());
        let mut remaining = scheduled_intents.into_iter();
        while !remaining.as_slice().is_empty() {
            let chunk_size = CHUNK_SIZE.min(remaining.len());
            let chunk = &remaining.as_slice()[..chunk_size];
            if let Err(err) = self.accept_chunk(chunk).await {
                error!(
                    intent_ids = ?chunk.iter().map(|intent| intent.intent_id).collect::<Vec<_>>(),
                    error = ?err,
                    "Failed to accept scheduled intents"
                );
                return Err((accepted, err));
            }
            accepted.extend(remaining.by_ref().take(chunk_size));
        }

        Ok(accepted)
    }

    pub(crate) async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, (Vec<ScheduledIntentBundle>, InternalOutboxClientError)>
    {
        let magic_context = self
            .engine
            .accounts()
            .loader()
            .read(&MAGIC_CONTEXT_PUBKEY, |account| {
                MagicContext::deserialize(account.data())
            })
            .map_err(|err| (vec![], err.into()))?
            .ok_or((vec![], InternalOutboxClientError::MagicContextMissing))?
            .map_err(|err| (vec![], err.into()))?;

        self.send_accept_tx(magic_context.scheduled_base_intents).await
    }

    pub(crate) fn outbox_reader(&self) -> InternalOutboxIntentBundlesReader {
        const CAPACITY: NonZeroUsize = NonZeroUsize::new(1000).unwrap();
        InternalOutboxIntentBundlesReader::new(self.engine.clone(), CAPACITY)
    }
}

#[async_trait]
impl OutboxClient for InternalOutboxClient {
    async fn set_intent_execution_stage(
        &self,
        intent_id: u64,
        stage: ExecutionStage,
        recovery_commit_nonces: Vec<(Pubkey, u64)>,
    ) -> Result<(), InternalOutboxClientError> {
        let ix = InstructionUtils::set_intent_execution_stage_instruction(
            self.engine.authority(),
            intent_id,
            stage,
            recovery_commit_nonces,
        );
        self.execute(&[ix]).await.inspect_err(|err| {
            error!(
                intent_id,
                error = ?err,
                "Failed to set intent execution stage"
            )
        })
    }

    async fn notify_commit_sent(
        &self,
        sent_commit: SentCommit,
    ) -> Result<(), InternalOutboxClientError> {
        let ix = InstructionUtils::scheduled_commit_sent_instruction(
            self.engine.authority(),
            sent_commit,
        )?;
        self.execute(&[ix])
            .await
            .inspect(|_| debug!("Sent commit signaled"))
            .inspect_err(|err| error!(error = ?err, "Failed to signal sent commit"))
    }

    async fn fetch_outbox_intent(
        &self,
        intent_id: u64,
    ) -> Result<Option<program::outbox_intent_bundles::OutboxIntentBundle>, InternalOutboxClientError>
    {
        fetch_outbox_intent(&self.engine, intent_id).map_err(Into::into)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalOutboxClientError {
    #[error("Outbox target {0} is not an empty system-owned mirror")]
    InvalidOutboxTarget(Pubkey),
    #[error("MagicContext account is missing from AccountsDb")]
    MagicContextMissing,
    #[error("TransactionError: {0}")]
    TransactionError(#[from] TransactionError),
    #[error("EngineError: {0}")]
    EngineError(#[from] EngineError),
    #[error("WincodeError: {0}")]
    WincodeError(#[from] wincode::ReadError),
    #[error("Receipt encoding error: {0}")]
    ReceiptEncoding(#[from] wincode::WriteError),
    #[error("AccountsDbError: {0}")]
    AccountsDbError(#[from] AccountsDBError),
    #[error(transparent)]
    Read(#[from] super::outbox_intent_bundles_reader::OutboxIntentBundlesReaderError),
    #[error("TransactionStatusMissing: {0}")]
    TransactionStatusMissing(Signature),
}
