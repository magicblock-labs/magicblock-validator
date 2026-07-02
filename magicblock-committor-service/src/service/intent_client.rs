use std::{mem, sync::Arc};

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
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use tracing::{debug, error, info};

use crate::{
    intent_execution_manager::BroadcastedIntentExecutionResult,
    intent_executor::ExecutionOutput,
};

/// Tracks the `ScheduledCommitSent` transaction for an in-flight intent.
///
/// Bundles recovered from persistence have no pre-built transaction: the
/// original blockhash is likely expired by restart time. `Recovered` signals
/// that the transaction must be constructed at notification time using a
/// fresh ER blockhash.
#[derive(Default)]
pub enum IntentSentTransaction {
    Known(Transaction),
    #[default]
    Recovered,
}

pub struct ScheduledBaseIntentMeta {
    slot: u64,
    blockhash: Hash,
    payer: Pubkey,
    included_pubkeys: Vec<Pubkey>,
    pub(crate) intent_sent_transaction: IntentSentTransaction,
    requested_undelegation: bool,
}

impl ScheduledBaseIntentMeta {
    pub fn new(intent: &ScheduledIntentBundle) -> Self {
        Self {
            slot: intent.slot,
            blockhash: intent.blockhash,
            payer: intent.payer,
            included_pubkeys: intent.get_all_committed_pubkeys(),
            intent_sent_transaction: if intent
                .sent_transaction
                .signatures
                .is_empty()
            {
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
    async fn send_accept_tx(&self) -> Result<(), InternalIntentClientError> {
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
impl<L: LatestBlockProvider> ERIntentClient for InternalIntentRpcClient<L> {
    type Error = InternalIntentClientError;

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

    async fn notify_commit_sent(
        &self,
        mut meta: ScheduledBaseIntentMeta,
        result: BroadcastedIntentExecutionResult,
    ) -> Result<(), Self::Error> {
        let intent_id = result.id;
        let tx = match mem::take(&mut meta.intent_sent_transaction) {
            IntentSentTransaction::Known(tx) => tx,
            IntentSentTransaction::Recovered => {
                let blockhash = self.latest_block_provider.blockhash();
                InstructionUtils::scheduled_commit_sent(intent_id, blockhash)
            }
        };
        let sent_commit = build_sent_commit(meta, &result);
        register_scheduled_commit_sent(sent_commit);
        let txn = with_encoded(tx).inspect_err(|err| {
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
