use super::backlog::BacklogDB;
use crate::{
    service::CommittorServiceError,
    shared::{
        account_state::{AccountSnapshot, CachedTaskInfoProvider, TaskInfoProvider},
        outbox::outbox_client::InternalOutboxClient,
    },
};
use chainlink::{AccountFetchEntrypoint, ProdChainlink};
use parking_lot::Mutex;
use program::outbox_intent_bundles::OutboxIntentBundle;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;
use std::{collections::HashSet, num::NonZeroUsize, sync::Arc};
use tokio::{
    sync::mpsc::{Sender, error::TrySendError},
    task,
};
use tracing::error;

pub(crate) struct IntentQueue {
    pub chainlink: Arc<ProdChainlink>,
    pub outbox_client: Arc<InternalOutboxClient>,
    pub task_info_provider: Arc<CachedTaskInfoProvider>,
    pub backlog: Arc<Mutex<dyn BacklogDB>>,
    pub intent_sender: Sender<OutboxIntentBundle>,
}

fn collect_undelegated_pubkeys(intent_bundles: &[OutboxIntentBundle]) -> Vec<Pubkey> {
    let mut pubkeys_being_undelegated = HashSet::<Pubkey>::new();
    intent_bundles.iter().for_each(|intent| {
        pubkeys_being_undelegated.extend(intent.get_undelegated_pubkeys());
    });
    pubkeys_being_undelegated.into_iter().collect::<Vec<_>>()
}

impl IntentQueue {
    fn enqueue(&self, intent_bundles: Vec<OutboxIntentBundle>) -> Result<(), IntentScheduleError> {
        let backlog = self.backlog.lock();
        let schedule_result = if backlog.is_empty() {
            let mut iter = intent_bundles.into_iter();
            // Treated as regular value not propagated lower
            #[allow(clippy::result_large_err)]
            let send_result = iter.try_for_each(|bundle| self.intent_sender.try_send(bundle));
            match send_result {
                Ok(_) => Ok(()),
                Err(TrySendError::Closed(_)) => Err(IntentScheduleError::ChannelClosed),
                Err(TrySendError::Full(bundle)) => {
                    let leftovers = std::iter::once(bundle).chain(iter).collect();
                    backlog.store_intent_bundles(leftovers).map_err(IntentScheduleError::from)
                }
            }
        } else {
            backlog.store_intent_bundles(intent_bundles).map_err(IntentScheduleError::from)
        };

        schedule_result.inspect_err(|err| {
            error!(error = ?err, "Failed to schedule intent");
        })?;

        Ok(())
    }
    pub(crate) async fn schedule(
        &self,
        bundles: Vec<OutboxIntentBundle>,
    ) -> Result<(), IntentScheduleError> {
        if bundles.is_empty() {
            return Ok(());
        }
        self.process_undelegation_requests(collect_undelegated_pubkeys(&bundles)).await;
        self.enqueue(bundles)
    }
    pub(crate) async fn reschedule_intents(&self) -> Result<(), CommittorServiceError> {
        /// Number of intents rescheduled at once
        const RESCHEDULE_CHUNK_SIZE: NonZeroUsize = NonZeroUsize::new(1000).unwrap();

        let mut outbox_bundles_reader = self.outbox_client.outbox_reader();
        loop {
            // Read by chunks in order not to overload the execution worker
            let mut intent_bundles_chunk =
                outbox_bundles_reader.read(RESCHEDULE_CHUNK_SIZE.get()).await?;
            if intent_bundles_chunk.is_empty() {
                return Ok(());
            }

            // Original blockhash is stale after restart; signal recovery so
            // notify_commit_sent rebuilds the tx with a fresh ER blockhash.
            intent_bundles_chunk
                .iter_mut()
                .for_each(|b| b.inner.sent_transaction = Transaction::default());

            let read_len = intent_bundles_chunk.len();
            let intent_bundles_chunk =
                self.retain_recoverable_outbox_intents(intent_bundles_chunk).await;
            // Schedule  without initial persistence as bundle already exists in db
            let result = self.schedule(intent_bundles_chunk).await;
            if let Err(err) = result {
                error!(error = ?err, "Failed to reschedule pending bundles")
            }

            // Check if we've rescheduled intents from outbox
            if read_len != RESCHEDULE_CHUNK_SIZE.get() {
                return Ok(());
            }
        }
    }
    async fn retain_recoverable_outbox_intents(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> Vec<OutboxIntentBundle> {
        let mut recoverable = Vec::with_capacity(intent_bundles.len());
        for bundle in intent_bundles {
            if self.is_outbox_intent_recoverable(&bundle).await {
                recoverable.push(bundle);
            }
        }
        recoverable
    }
    async fn is_outbox_intent_recoverable(&self, bundle: &OutboxIntentBundle) -> bool {
        if !self.is_same_delegation_session(bundle).await {
            return false;
        }

        self.has_valid_recovery_nonces(bundle).await
    }
    async fn is_same_delegation_session(&self, bundle: &OutboxIntentBundle) -> bool {
        let recovered_accounts = bundle.get_all_committed_accounts();
        if recovered_accounts.is_empty() {
            return true;
        }

        let pubkeys = recovered_accounts.iter().map(|account| account.pubkey).collect::<Vec<_>>();
        let current_sessions = match self
            .chainlink
            .account_delegation_sessions(&pubkeys, AccountFetchEntrypoint::Internal)
            .await
        {
            Ok(sessions) => sessions,
            Err(err) => {
                error!(
                    intent_id = bundle.intent_id,
                    error = ?err,
                    "Skipping outbox recovery after delegation session lookup failed"
                );
                return false;
            }
        };

        recovered_accounts.iter().zip(current_sessions).all(|(recovered, current)| {
            let Some(current) = current else {
                error!(
                    intent_id = bundle.intent_id,
                    pubkey = %recovered.pubkey,
                    "Skipping outbox recovery because committed account is missing locally"
                );
                return false;
            };
            let same_session = current.locally_protected
                && (recovered.remote_slot == 0 || recovered.remote_slot == current.remote_slot);
            if !same_session {
                error!(
                    intent_id = bundle.intent_id,
                    pubkey = %recovered.pubkey,
                    recovered_slot = recovered.remote_slot,
                    current_slot = current.remote_slot,
                    locally_protected = current.locally_protected,
                    "Skipping outbox recovery because delegation session changed"
                );
            }
            same_session
        })
    }
    async fn has_valid_recovery_nonces(&self, bundle: &OutboxIntentBundle) -> bool {
        let recovery_nonces = bundle.recovery_commit_nonces();
        if recovery_nonces.is_empty() {
            return true;
        }

        let committed_accounts = bundle
            .get_all_committed_accounts()
            .into_iter()
            .map(|account| (account.pubkey, account.remote_slot))
            .collect::<Vec<AccountSnapshot>>();
        let min_context_slot =
            committed_accounts.iter().map(|(_, slot)| *slot).max().unwrap_or_default();
        let current_nonces = match self
            .task_info_provider
            .fetch_current_commit_nonces(&committed_accounts, min_context_slot)
            .await
        {
            Ok(nonces) => nonces,
            Err(err) => {
                error!(
                    intent_id = bundle.intent_id,
                    error = ?err,
                    "Skipping outbox recovery after commit nonce lookup failed"
                );
                return false;
            }
        };

        recovery_nonces.iter().all(|(pubkey, recovery_commit_nonce)| {
            let Some(current_commit_nonce) = current_nonces.get(pubkey) else {
                error!(
                    intent_id = bundle.intent_id,
                    %pubkey,
                    "Skipping outbox recovery because current commit nonce is missing"
                );
                return false;
            };
            let valid = recovery_commit_nonce >= current_commit_nonce;
            if !valid {
                error!(
                    intent_id = bundle.intent_id,
                    %pubkey,
                    recovery_commit_nonce,
                    current_commit_nonce,
                    "Skipping stale outbox recovery because chain nonce has advanced"
                );
            }
            valid
        })
    }
    async fn process_undelegation_requests(&self, pubkeys: Vec<Pubkey>) {
        let mut join_set = task::JoinSet::new();
        for pubkey in pubkeys.into_iter() {
            let chainlink = self.chainlink.clone();
            join_set.spawn(async move { (pubkey, chainlink.undelegation_requested(pubkey).await) });
        }
        let sub_errors = join_set
            .join_all()
            .await
            .into_iter()
            .filter_map(|(pubkey, inner_result)| {
                if let Err(err) = inner_result {
                    Some(format!("Subscribing to account {} failed: {}", pubkey, err))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if !sub_errors.is_empty() {
            // Instead of aborting the entire commit we log an error here, however
            // this means that the undelegated accounts stay in a problematic state
            // in the validator and are not synced from chain.
            // We could implement a retry mechanism inside of chainlink in the future.
            error!(
                error_count = sub_errors.len(),
                "Failed to subscribe to accounts being undelegated"
            );
        }
    }
}
#[derive(thiserror::Error, Debug)]
pub(crate) enum IntentScheduleError {
    #[error("Execution worker failed: {0}")]
    Worker(#[from] task::JoinError),
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] super::backlog::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mbv_core::intent::{
        CommitAndUndelegate, CommitType, MagicIntentBundle, UndelegateType,
        outbox::outbox_intent_pda_with_bump, types::CommittedAccount,
    };
    use program::magic_scheduled_base_intent::ScheduledIntentBundle;
    use solana_account::Account;
    use solana_hash::Hash;

    use super::*;

    fn committed_account(pubkey: Pubkey) -> CommittedAccount {
        CommittedAccount {
            pubkey,
            account: Account::default(),
            remote_slot: Default::default(),
        }
    }

    fn accepted_bundle(intent_id: u64, intent_bundle: MagicIntentBundle) -> OutboxIntentBundle {
        let intent = ScheduledIntentBundle {
            intent_id,
            slot: 0,
            blockhash: Hash::default(),
            sent_transaction: Default::default(),
            payer: Pubkey::default(),
            intent_bundle,
        };
        let bump = outbox_intent_pda_with_bump(intent_id).1;
        OutboxIntentBundle::accepted(intent, bump)
    }

    #[test]
    fn collect_undelegated_pubkeys_includes_commit_finalize_and_undelegate() {
        let committed_only = Pubkey::new_unique();
        let commit_and_undelegate = Pubkey::new_unique();
        let commit_finalize_and_undelegate = Pubkey::new_unique();

        let intent_bundle = MagicIntentBundle {
            commit: Some(CommitType::Standalone(vec![committed_account(
                committed_only,
            )])),
            commit_and_undelegate: Some(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![committed_account(
                    commit_and_undelegate,
                )]),
                undelegate_action: UndelegateType::Standalone,
            }),
            commit_finalize_and_undelegate: Some(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![committed_account(
                    commit_finalize_and_undelegate,
                )]),
                undelegate_action: UndelegateType::Standalone,
            }),
            ..Default::default()
        };

        let actual = collect_undelegated_pubkeys(&[accepted_bundle(1, intent_bundle)])
            .into_iter()
            .collect::<HashSet<_>>();

        assert_eq!(
            actual,
            HashSet::from([commit_and_undelegate, commit_finalize_and_undelegate])
        );
        assert!(!actual.contains(&committed_only));
    }
}
