use std::{collections::HashMap, time::Duration};

use mbv_core::{
    intent::BaseActionCallback,
    traits::{ActionError, ActionResult, ActionsCallbackScheduler},
};
use program::outbox::PendingTransaction;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use tokio::time::sleep;
use tracing::{error, info};

use crate::{
    intent_executor::{
        IntentExecutionReport,
        error::{IntentExecutorError, IntentExecutorResult},
        intent_execution_client::IntentExecutionClient,
    },
    tasks::{
        Task,
        task_builder::TaskBuilderError,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        task_strategist::{TaskStrategist, TransactionStrategy},
    },
    transaction_preparator::{
        TransactionPreparator, cleanup_resources::CleanupResources,
        error::TransactionPreparatorError,
    },
};

/// How long to wait between re-checks of a pending signature that is still
/// within its blockhash's valid window (i.e. it might still land).
const PENDING_SIGNATURE_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub(in crate::intent_executor) fn recovery_commit_nonces(tasks: &[Task]) -> Vec<(Pubkey, u64)> {
    tasks
        .iter()
        .filter_map(|task| match task {
            Task::CommitFinalize(task) => Some((task.committed_account.pubkey, task.commit_id)),
            _ => None,
        })
        .collect()
}

pub async fn prepare_transaction<T: TransactionPreparator>(
    client: &IntentExecutionClient,
    authority: &Keypair,
    transaction_preparator: &T,
    transaction_strategy: &TransactionStrategy,
    cleanup: &mut CleanupResources,
) -> Result<VersionedTransaction, TransactionPreparatorError> {
    let mut prepared_message = transaction_preparator
        .prepare_for_strategy(authority, transaction_strategy, cleanup)
        .await?;

    // Get latest blockhash(Part of preparation I guess)
    let blockhash = client
        .get_latest_blockhash()
        .await
        .map_err(TransactionPreparatorError::GetLatestBlockhashError)?;
    prepared_message.set_recent_blockhash(blockhash);

    // Create and sign transaction(Part of transaction preparation i guess, hence the errors)
    let transaction = VersionedTransaction::try_new(prepared_message, &[&authority])
        .map_err(TransactionPreparatorError::SignerError)?;
    Ok(transaction)
}

pub(in crate::intent_executor) enum PendingSignatureStatus {
    /// The transaction landed successfully.
    Succeeded,
    /// Not found on-chain yet, but its blockhash is still within its valid
    /// window - it may still land. Callers must not resend.
    Pending,
    /// The transaction landed and failed, or its blockhash has expired
    /// without landing - guaranteed dead, safe to send a new transaction.
    Rejected,
    /// The RPC returned no entry at all for the single signature we asked
    /// about - a contract violation, not a normal "not found yet" response.
    /// Distinct from `Pending` so repeated occurrences can be bounded
    /// instead of retried forever.
    Anomalous,
}

/// Checks a pending signature loaded from the outbox against the full
/// transaction history. A signature that isn't found on-chain is ambiguous
/// on its own — Solana's RPC can't tell "never sent" apart from "sent but
/// not yet landed" — so we additionally check whether its blockhash is
/// still valid to decide between `Pending` (may still land) and `Rejected`
/// (guaranteed dead).
///
/// Blockhash validity is checked *before* signature status, not after.
/// Validity is monotonic (once expired, it stays expired), so a
/// slightly-stale "expired" reading is still safe to act on later. But
/// "signature not found" is a snapshot that can flip to "found" at any
/// later instant — if we checked it first and the blockhash-expired check
/// second, the tx could land in the gap between the two calls and we'd
/// wrongly conclude `Rejected`, causing a double execution.
pub(in crate::intent_executor) async fn check_pending_signature(
    client: &IntentExecutionClient,
    pending: &PendingTransaction,
) -> IntentExecutorResult<PendingSignatureStatus> {
    let blockhash_still_valid = client
        .is_blockhash_valid(&pending.blockhash)
        .await
        .map_err(IntentExecutorError::GetPendingSignatureStatusError)?;

    let statuses = client
        .get_signature_statuses_with_history_at_commitment(
            std::slice::from_ref(&pending.signature),
            CommitmentConfig::finalized(),
        )
        .await
        .map_err(IntentExecutorError::GetPendingSignatureStatusError)?;

    match statuses.first() {
        Some(Some(Ok(()))) => Ok(PendingSignatureStatus::Succeeded),
        Some(Some(Err(err))) => {
            info!(pending_signature = ?pending.signature, error = ?err, "Transaction corresponding to pending signature failed");
            Ok(PendingSignatureStatus::Rejected)
        }
        // Not found on-chain (but present in the response): could be
        // genuinely never sent, or simply not landed yet.
        Some(None) => {
            if blockhash_still_valid {
                Ok(PendingSignatureStatus::Pending)
            } else {
                info!(pending_signature = ?pending.signature, "Pending transaction's blockhash expired without landing");
                Ok(PendingSignatureStatus::Rejected)
            }
        }
        // We requested exactly one signature, so a response with no entry
        // at all violates the RPC contract - the node is misbehaving. Report
        // `Anomalous` rather than `Rejected`, since wrongly concluding the tx
        // is dead risks a double-send/double-execution; callers bound how
        // many consecutive anomalies they tolerate before giving up.
        None => {
            error!(pending_signature = ?pending.signature, "RPC returned no entry for requested signature status");
            Ok(PendingSignatureStatus::Anomalous)
        }
    }
}

/// Waits until a pending signature resolves definitively, sleeping and
/// re-checking while `Pending` (naturally bounded by the blockhash's own
/// expiry window). Returns `true` if the transaction landed successfully,
/// `false` if it's guaranteed dead (failed on-chain or blockhash expired).
/// Callers that don't already sit inside a retry loop (e.g. resuming after
/// a restart) should use this instead of calling `check_pending_signature`
/// directly, to avoid acting on a `Pending` result as if it were safe to
/// resend.
pub(in crate::intent_executor) async fn resolve_pending_signature(
    client: &IntentExecutionClient,
    pending: &PendingTransaction,
) -> IntentExecutorResult<bool> {
    /// Consecutive `Anomalous` responses tolerated before giving up -
    /// genuine `Pending` is unbounded (it already ends once the blockhash
    /// expires), this only guards against a misbehaving RPC that never
    /// returns a usable answer.
    const MAX_CONSECUTIVE_ANOMALIES: u8 = 5;

    let mut consecutive_anomalies = 0u8;
    loop {
        match check_pending_signature(client, pending).await? {
            PendingSignatureStatus::Succeeded => return Ok(true),
            PendingSignatureStatus::Rejected => return Ok(false),
            PendingSignatureStatus::Pending => {
                consecutive_anomalies = 0;
            }
            PendingSignatureStatus::Anomalous => {
                consecutive_anomalies += 1;
            }
        }

        if consecutive_anomalies >= MAX_CONSECUTIVE_ANOMALIES {
            break Err(IntentExecutorError::PendingSignatureResolutionError(
                pending.signature,
            ));
        } else {
            sleep(PENDING_SIGNATURE_POLL_INTERVAL).await;
        };
    }
}

/// Handles out of sync commit id error, fixes current strategy
pub(in crate::intent_executor) async fn handle_commit_id_error<T: TaskInfoFetcher>(
    authority: &Pubkey,
    task_info_fetcher: &CacheTaskInfoFetcher<T>,
    committed_pubkeys: &[Pubkey],
    strategy: &mut TransactionStrategy,
    intent_id: u64,
) -> Result<(), TaskBuilderError> {
    let snapshot_slots: HashMap<Pubkey, u64> = strategy
        .tasks
        .iter()
        .filter_map(|task| match task {
            Task::CommitFinalize(task) => Some((
                task.committed_account.pubkey,
                task.committed_account.remote_slot,
            )),
            _ => None,
        })
        .collect();
    let min_context_slot = snapshot_slots.values().copied().max().unwrap_or_default();
    let committed_accounts: Vec<_> = committed_pubkeys
        .iter()
        .map(|pubkey| {
            let slot = snapshot_slots.get(pubkey).copied().unwrap_or(min_context_slot);
            (*pubkey, slot)
        })
        .collect();

    // We reset TaskInfoFetcher for all committed accounts
    // We re-fetch them to fix out of sync tasks
    task_info_fetcher.invalidate_nonces(committed_pubkeys);
    let commit_ids = task_info_fetcher
        .fetch_next_commit_nonces(&committed_accounts, min_context_slot)
        .await
        .map_err(TaskBuilderError::CommitTasksBuildError)?;

    // Old buffer identities remain in the preparation attempt's cleanup record.
    for task in &mut strategy.tasks {
        if let Task::CommitFinalize(task) = task {
            let Some(commit_id) = commit_ids.get(&task.committed_account.pubkey) else {
                continue;
            };
            task.commit_id = *commit_id;
        }
    }

    // Re-fetched nonces land back on 1 after a re-delegation; such a retry is
    // a first-commit transaction and must carry the uniqueness noop.
    if requires_uniqueness_nonce(&strategy.tasks) {
        strategy.uniqueness_nonce = Some(intent_id);
    }

    strategy.refresh_lookup_table_keys(authority);
    Ok(())
}

/// On the first commit of a delegation instance the nonce restarts at 1, so the
/// transaction can be byte-identical to a prior instance's landed commit and
/// alias its signature. Such intents must carry a per-intent uniqueness noop.
pub(in crate::intent_executor) fn requires_uniqueness_nonce(commit_tasks: &[Task]) -> bool {
    commit_tasks.iter().any(|task| match task {
        Task::CommitFinalize(task) => task.commit_id <= 1,
        _ => false,
    })
}

/// Splits a transaction at its last commit, retaining later tasks in the follow-up.
pub(in crate::intent_executor) fn split_for_follow_up(
    authority: &Pubkey,
    strategy: TransactionStrategy,
) -> (TransactionStrategy, TransactionStrategy) {
    // Keep tasks through the last commit together; the remaining tasks form the follow-up.
    let last_commit_ind =
        strategy.tasks.iter().rposition(|el| matches!(el, Task::CommitFinalize(_)));
    let (mut commit_tasks, mut follow_up_tasks) = (vec![], vec![]);
    for (i, el) in strategy.tasks.into_iter().enumerate() {
        if Some(i) <= last_commit_ind {
            commit_tasks.push(el);
        } else {
            follow_up_tasks.push(el);
        }
    }

    let commit_alt_pubkeys = if strategy.lookup_tables_keys.is_empty() {
        vec![]
    } else {
        TaskStrategist::collect_lookup_table_keys(
            authority,
            &commit_tasks,
            strategy.uniqueness_nonce,
        )
    };
    let commit_strategy = TransactionStrategy {
        tasks: commit_tasks,
        lookup_tables_keys: commit_alt_pubkeys,
        uniqueness_nonce: strategy.uniqueness_nonce,
    };

    let follow_up_alt_pubkeys = if strategy.lookup_tables_keys.is_empty() {
        vec![]
    } else {
        TaskStrategist::collect_lookup_table_keys(
            authority,
            &follow_up_tasks,
            strategy.uniqueness_nonce,
        )
    };
    let follow_up_strategy = TransactionStrategy {
        tasks: follow_up_tasks,
        lookup_tables_keys: follow_up_alt_pubkeys,
        uniqueness_nonce: strategy.uniqueness_nonce,
    };

    (commit_strategy, follow_up_strategy)
}

/// Handles undelegation error, stripping away actions
pub(in crate::intent_executor) fn handle_undelegation_error(
    authority: &Pubkey,
    strategy: &mut TransactionStrategy,
) -> Vec<BaseActionCallback> {
    let position = strategy.tasks.iter().position(|el| matches!(el, Task::Undelegate(_)));

    if let Some(position) = position {
        // Remove everything after undelegation including post undelegation actions
        let callbacks = strategy
            .tasks
            .drain(position..)
            .filter_map(|task| match task {
                Task::BaseAction(mut action) => action.extract_callback(),
                _ => None,
            })
            .collect();
        strategy.refresh_lookup_table_keys(authority);
        callbacks
    } else {
        Vec::new()
    }
}

pub(in crate::intent_executor) fn handle_actions_result<A>(
    authority: &Pubkey,
    callback_scheduler: &A,
    execution_report: &mut IntentExecutionReport,
    transaction_strategy: &mut TransactionStrategy,
    signature: Option<Signature>,
    result: ActionResult,
) where
    A: ActionsCallbackScheduler,
{
    let callbacks = match result {
        // TODO(edwin): add in PR description
        // Timeout may be triggered right after tx was sent
        // It could also be triggered prior
        // In order to give simple contract to user we keep actions in both cases
        Ok(()) | Err(ActionError::TimeoutError) => transaction_strategy.extract_action_callbacks(),
        _ => transaction_strategy.remove_actions(authority),
    };
    if !callbacks.is_empty() {
        let result = callback_scheduler.schedule(callbacks, signature, result);
        execution_report.add_callback_report(result);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use async_trait::async_trait;
    use delegation_program_api::state::{DelegationMetadata, UndelegationRequester};
    use mbv_core::intent::types::CommittedAccount;
    use solana_account::Account;

    use super::*;
    use crate::tasks::{
        task_info_fetcher::{AccountSnapshot, TaskInfoFetcherResult},
        utils::create_commit_finalize_task,
    };

    /// Reports commit id 1 except for one unchanged account at commit id 5.
    struct FreshDelegationFetcher(Option<Pubkey>);

    #[async_trait]
    impl TaskInfoFetcher for FreshDelegationFetcher {
        async fn fetch_next_commit_nonces(
            &self,
            accounts: &[AccountSnapshot],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(accounts
                .iter()
                .map(|(pubkey, _)| (*pubkey, if self.0 == Some(*pubkey) { 5 } else { 1 }))
                .collect())
        }

        async fn fetch_current_commit_nonces(
            &self,
            accounts: &[AccountSnapshot],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(accounts
                .iter()
                .map(|(pubkey, _)| (*pubkey, if self.0 == Some(*pubkey) { 4 } else { 0 }))
                .collect())
        }

        async fn fetch_delegation_metadata(
            &self,
            accounts: &[AccountSnapshot],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, DelegationMetadata>> {
            Ok(accounts
                .iter()
                .map(|(pubkey, _)| {
                    (
                        *pubkey,
                        DelegationMetadata {
                            last_commit_id: 0,
                            undelegation_requester: UndelegationRequester::None,
                            seeds: vec![],
                            rent_payer: *pubkey,
                        },
                    )
                })
                .collect())
        }

        async fn get_base_accounts(
            &self,
            _: &[Pubkey],
            _: u64,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
            Ok(Default::default())
        }
    }

    #[tokio::test]
    async fn test_mixed_commit_id_recovery_sets_uniqueness_nonce() {
        let pubkey = Pubkey::new_unique();
        let unchanged_pubkey = Pubkey::new_unique();
        // Stale cache produced commit id 5; on chain the account was
        // re-delegated, so the correct commit id is 1.
        let stale_task = create_commit_finalize_task(
            5,
            false,
            CommittedAccount {
                pubkey,
                account: Account::default(),
                remote_slot: Default::default(),
            },
            None,
        );
        let mut strategy = TransactionStrategy {
            tasks: vec![
                stale_task.into(),
                create_commit_finalize_task(
                    5,
                    false,
                    CommittedAccount {
                        pubkey: unchanged_pubkey,
                        account: Account::default(),
                        remote_slot: Default::default(),
                    },
                    None,
                )
                .into(),
            ],
            lookup_tables_keys: vec![],
            uniqueness_nonce: None,
        };

        let fetcher = CacheTaskInfoFetcher::new(FreshDelegationFetcher(Some(unchanged_pubkey)));
        handle_commit_id_error(
            &Pubkey::new_unique(),
            &fetcher,
            &[pubkey, unchanged_pubkey],
            &mut strategy,
            42,
        )
        .await
        .expect("commit id recovery succeeds");

        assert_eq!(strategy.uniqueness_nonce, Some(42));
        let ids: Vec<_> = recovery_commit_nonces(&strategy.tasks);
        assert_eq!(ids, vec![(pubkey, 1), (unchanged_pubkey, 5)]);
    }
}
