use std::mem;

use async_trait::async_trait;
use engine::{Engine, IntoTransactionView};
use magicblock_program::{
    MAGIC_CONTEXT_PUBKEY, MagicContext, SentCommit, TransactionScheduler, id,
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    register_scheduled_commit_sent,
};
use solana_account::ReadableAccount;
use solana_hash::Hash;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use tracing::{debug, error, info};

use crate::{
    intent_execution_manager::BroadcastedIntentExecutionResult,
    intent_executor::ExecutionOutput,
};

pub struct ScheduledBaseIntentMeta {
    slot: u64,
    blockhash: Hash,
    payer: Pubkey,
    included_pubkeys: Vec<Pubkey>,
    sent_transaction: IntentSentTransaction,
    requested_undelegation: bool,
}

#[derive(Default)]
enum IntentSentTransaction {
    Known(Transaction),
    #[default]
    Recovered,
}

impl ScheduledBaseIntentMeta {
    pub fn new(intent: &ScheduledIntentBundle) -> Self {
        Self {
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent.get_all_committed_pubkeys(),
            sent_transaction: if intent.sent_transaction.signatures.is_empty() {
                IntentSentTransaction::Recovered
            } else {
                IntentSentTransaction::Known(intent.sent_transaction.clone())
            },
            requested_undelegation: intent.has_undelegate_intent(),
        }
    }
}

#[async_trait]
pub trait ERIntentClient: Send + Sync + 'static {
    type Error: std::error::Error + Send;

    /// Executes `Accept` tx and returns accepted intents
    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, Self::Error>;

    /// Processes intent results, submitting them on chain(ER)
    async fn notify_commit_sent(
        &self,
        meta: ScheduledBaseIntentMeta,
        result: BroadcastedIntentExecutionResult,
    ) -> Result<(), Self::Error>;

    // TODO(edwin): probably more proper place to load pending intent
    // CommittorProcessor::pending_intent_bundles could be moved here in the future
}

/// Reads the MagicContext, builds ER transactions against the engine's latest
/// blockhash, and submits them through the engine.
pub struct InternalIntentRpcClient {
    engine: Engine,
}

impl InternalIntentRpcClient {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }

    /// Composes `message` into a transaction, signs it with the engine's
    /// authority, submits it, and awaits its committed result.
    async fn execute<T: IntoTransactionView>(
        &self,
        transaction: T,
    ) -> Result<(), InternalIntentClientError> {
        self.engine
            .transaction(transaction)
            .map_err(|err| {
                error!(error = ?err, "Failed to compose intent transaction");
                TransactionError::SanitizeFailure
            })?
            .execute()
            .await
            .map_err(|err| {
                error!(error = ?err, "Failed to execute intent transaction");
                TransactionError::ClusterMaintenance
            })??;
        Ok(())
    }

    /// Sends transaction to move the scheduled commits from the `MagicContext`
    /// to the global ScheduledCommit store
    async fn send_accept_tx(&self) -> Result<(), InternalIntentClientError> {
        let authority = self.engine.authority();
        let ix =
            InstructionUtils::accept_scheduled_commits_instruction(&authority);
        let message = Message::new(&[ix], Some(&authority));
        self.execute(message).await.inspect_err(|err| {
            error!(error = ?err, "Failed to accept scheduled commits");
        })
    }
}

#[async_trait]
impl ERIntentClient for InternalIntentRpcClient {
    type Error = InternalIntentClientError;

    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, Self::Error> {
        // If accounts were scheduled to be committed, we accept them here
        // and processs the commits
        let has_scheduled_commits = self
            .engine
            .accounts()
            .loader()
            .read(&MAGIC_CONTEXT_PUBKEY, |account| {
                MagicContext::has_scheduled_commits(account.data())
            })
            .ok()
            .flatten()
            .expect(
                "Validator found to be running without MagicContext account!",
            );
        if !has_scheduled_commits {
            return Ok(vec![]);
        }
        self.send_accept_tx().await?;

        // Return intents from global store
        Ok(TransactionScheduler::default().take_scheduled_intent_bundles())
    }

    async fn notify_commit_sent(
        &self,
        mut meta: ScheduledBaseIntentMeta,
        result: BroadcastedIntentExecutionResult,
    ) -> Result<(), Self::Error> {
        let intent_id = result.id;
        let transaction = mem::take(&mut meta.sent_transaction);
        let sent_commit = build_sent_commit(meta, &result);
        register_scheduled_commit_sent(sent_commit);
        let result = match transaction {
            IntentSentTransaction::Known(transaction) => {
                self.execute(transaction).await
            }
            IntentSentTransaction::Recovered => {
                let authority = self.engine.authority();
                let ix = InstructionUtils::scheduled_commit_sent_instruction(
                    &id(),
                    &authority,
                    intent_id,
                );
                let message = Message::new(&[ix], Some(&authority));
                self.execute(message).await
            }
        };
        result
            .inspect(|_| debug!("Sent commit signaled"))
            .inspect_err(
                |err| error!(error = ?err, "Failed to signal sent commit"),
            )?;

        Ok(())
    }
}

fn build_sent_commit(
    meta: ScheduledBaseIntentMeta,
    result: &BroadcastedIntentExecutionResult,
) -> SentCommit {
    let intent_id = result.id;
    let error_message = result.as_ref().err().map(|err| format!("{:?}", err));
    let chain_signatures = match &result.inner {
        Ok(value) => match value {
            ExecutionOutput::SingleStage(signature) => vec![*signature],
            ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } => vec![*commit_signature, *finalize_signature],
        },
        Err(err) => {
            error!(
                "Failed to commit intent: {}, slot: {}, blockhash: {}. {:?}",
                intent_id, meta.slot, meta.blockhash, err
            );
            err.signatures()
                .map(|(commit, finalize)| {
                    finalize
                        .map(|finalize| vec![commit, finalize])
                        .unwrap_or(vec![commit])
                })
                .unwrap_or_default()
        }
    };
    let patched_errors = result
        .patched_errors
        .iter()
        .map(|err| {
            info!("Patched intent: {}. error was: {}", intent_id, err);
            err.to_string()
        })
        .collect();

    let callbacks_report = result
        .callbacks_report
        .iter()
        .map(|r| match r {
            Ok(sig) => {
                format!("OK: {sig}")
            }
            Err(err) => {
                error!(
                    "Callback failed to schedule: {}. error: {}",
                    intent_id, err
                );
                format!("ERR: {err}")
            }
        })
        .collect();

    SentCommit {
        message_id: intent_id,
        slot: meta.slot,
        blockhash: meta.blockhash,
        payer: meta.payer,
        chain_signatures,
        included_pubkeys: meta.included_pubkeys,
        excluded_pubkeys: vec![],
        requested_undelegation: meta.requested_undelegation,
        error_message,
        patched_errors,
        callbacks_scheduling_results: callbacks_report,
    }
}

#[derive(thiserror::Error, Debug)]
pub enum InternalIntentClientError {
    #[error("TransactionError: {0}")]
    TransactionError(#[from] TransactionError),
}
