//! Update listening, slot-matched resolution, and the replay outbox.

use std::{
    collections::hash_map::Entry,
    sync::{atomic::Ordering, Arc},
};

pub(crate) use chain_pubsub_client::ChainPubsubClient;
pub(crate) use chain_rpc_client::ChainRpcClient;
pub(crate) use errors::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};
use magicblock_metrics::{
    metrics,
    metrics::{
        AccountFetchContext, ChainlinkCompanionFetchOutcome,
        ChainlinkPendingFetchLayer, ChainlinkPendingFetchOutcome,
    },
};
pub(crate) use remote_account::RemoteAccount;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::clock;
use tokio::{sync::mpsc, task};
use tracing::*;

use super::*;
use crate::remote_account_provider::pubsub_common::{
    SubscriptionSource, SubscriptionUpdate,
};

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    pub(super) fn listen_for_account_updates(
        &self,
        mut updates: mpsc::Receiver<SubscriptionUpdate>,
    ) -> RemoteAccountProviderResult<()> {
        let fetching_accounts = self.fetching_accounts.clone();
        let chain_slot = self.chain_slot.clone();
        let received_updates_count = self.received_updates_count.clone();
        let last_update_slot = self.last_update_slot.clone();
        let subscription_forwarder = self.subscription_forwarder.clone();
        let subscription_tiers = Arc::new(self.subscription_tier_ctx());
        task::spawn(async move {
            while let Some(update) = updates.recv().await {
                let slot = update.slot;

                received_updates_count.fetch_add(1, Ordering::Relaxed);
                last_update_slot.store(slot, Ordering::Relaxed);

                if update.pubkey == clock::ID {
                    // We show as part of test_chain_pubsub_client_clock that the response
                    // context slot always matches the slot encoded in the slot data.
                    // Use fetch_max to ensure we always keep the highest slot value,
                    // since GRPC may have already updated chain_slot to a higher value.
                    chain_slot.update(slot);
                    // NOTE: we do not forward clock updates
                } else {
                    trace!(
                        pubkey = %update.pubkey,
                        slot,
                        "Received account update"
                    );
                    let remote_account = match update.account {
                        Some(account) => RemoteAccount::from_fresh_account(
                            account,
                            slot,
                            RemoteAccountUpdateSource::Subscription,
                        ),
                        None => {
                            warn!(
                                pubkey = %update.pubkey,
                                "Account update could not be decoded"
                            );
                            RemoteAccount::NotFound(slot)
                        }
                    };

                    let account_is_found = remote_account.is_found();

                    // Fast path: fetch arbitration and tier movement only
                    // apply while a fetch is pending or the account sits in
                    // the secondary tier. All other updates forward without
                    // taking the per-key guard or the transition lock.
                    let needs_tier_handling =
                        subscription_tiers.secondary.contains(&update.pubkey)
                            || fetching_accounts
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .contains_key(&update.pubkey);

                    // Serialize fetch arbitration and tier movement so a late
                    // RPC result cannot overwrite this subscription update.
                    let mut classification_error = None;
                    let (forward_update, _accepted_update, resolved_fetch) =
                        if !needs_tier_handling {
                            // Record so a lagging RPC result cannot later win
                            // classification against this newer update.
                            if account_is_found {
                                subscription_tiers
                                    .record_classification(
                                        update.pubkey,
                                        slot,
                                        SubscriptionClassificationSource::Subscription,
                                    )
                                    .await;
                            }
                            (
                                Some(ForwardedSubscriptionUpdate {
                                    pubkey: update.pubkey,
                                    account: remote_account.clone(),
                                    source: update.source,
                                }),
                                true,
                                None,
                            )
                        } else {
                            // The per-key guard serializes this update
                            // against fetch resolutions and other transitions
                            // of the same key; the tier helpers scope the
                            // transition lock to their in-memory critical
                            // sections.
                            let _subscription_guard =
                                subscription_key_owned_guard_from_map(
                                    &subscription_tiers.subscription_key_locks,
                                    update.pubkey,
                                )
                                .await;
                            let classification_is_current = subscription_tiers
                                .classification_is_current(
                                    update.pubkey,
                                    slot,
                                    SubscriptionClassificationSource::Subscription,
                                )
                                .await;
                            let result = if classification_is_current {
                                let mut fetching =
                                    fetching_accounts.lock().unwrap_or_else(
                                        |poison| poison.into_inner(),
                                    );
                                if let Some(generation) = fetching
                                    .get(&update.pubkey)
                                    .map(|state| state.generation)
                                {
                                    if let Some(state) =
                                    remove_fetching_account_if_generation_matches(
                                        &mut fetching,
                                        &update.pubkey,
                                        generation,
                                    )
                                {
                                    // If subscription update is newer than when we started fetching,
                                    // resolve with the subscription data instead
                                    if slot >= state.fetch_start_slot {
                                        trace!(pubkey = %update.pubkey, slot = slot, fetch_start_slot = state.fetch_start_slot, generation, "Using subscription update instead of fetch");
                                        metrics::observe_chainlink_pending_fetch_owner_duration_seconds_with_context(
                                            state.fetch_context.clone(),
                                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                                            ChainlinkPendingFetchOutcome::ResolvedBySubscriptionUpdate,
                                            state.owner_started_at.elapsed().as_secs_f64(),
                                        );
                                        metrics::inc_chainlink_pending_fetch_accounts_with_context(
                                            state.fetch_context.clone(),
                                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                                            ChainlinkPendingFetchOutcome::ResolvedBySubscriptionUpdate,
                                            1,
                                        );

                                        // Also forward: the fetch waiters may
                                        // not clone the result (e.g. status
                                        // reads) and dedup already dropped
                                        // every other copy of this update.
                                        (
                                            Some(ForwardedSubscriptionUpdate {
                                                pubkey: update.pubkey,
                                                account: remote_account.clone(),
                                                source: update.source,
                                            }),
                                            true,
                                            Some((generation, state.waiters)),
                                        )
                                    } else {
                                        // Subscription is stale, put the fetch tracking back
                                        debug!(pubkey = %update.pubkey, slot = slot, fetch_start_slot = state.fetch_start_slot, generation, "Received stale subscription update");
                                        fetching.insert(update.pubkey, state);
                                        (None, false, None)
                                    }
                                } else {
                                    (None, false, None)
                                }
                                } else {
                                    (
                                        Some(ForwardedSubscriptionUpdate {
                                            pubkey: update.pubkey,
                                            account: remote_account.clone(),
                                            source: update.source,
                                        }),
                                        true,
                                        None,
                                    )
                                }
                            } else {
                                debug!(pubkey = %update.pubkey, slot, "Ignoring stale subscription classification");
                                (None, false, None)
                            };

                            // The in-flight acquisition may not have created
                            // the ownership entry yet; record so the later
                            // RPC result loses arbitration.
                            let apply_classification = result.1
                                && account_is_found
                                && match result.2.as_ref() {
                                    Some((generation, _)) => {
                                        subscription_tiers
                                            .record_classification_for_pending_fetch(
                                                update.pubkey,
                                                slot,
                                                SubscriptionClassificationSource::Subscription,
                                                *generation,
                                            )
                                            .await
                                    }
                                    None => {
                                        subscription_tiers
                                            .record_classification(
                                                update.pubkey,
                                                slot,
                                                SubscriptionClassificationSource::Subscription,
                                            )
                                            .await
                                    }
                                };
                            if apply_classification
                                && !subscription_tiers
                                    .secondary
                                    .contains(&update.pubkey)
                                && result.2.is_some()
                                && !subscription_tiers
                                    .primary
                                    .contains(&update.pubkey)
                            {
                                // The pending fetch resolved before its
                                // subscription setup created any tier state;
                                // admit the found account into the primary
                                // tier now so it is never handed to waiters
                                // without primary admission.
                                if let Err(err) = subscription_tiers
                                    .admit_resolved_fetch_to_primary(
                                        update.pubkey,
                                    )
                                    .await
                                {
                                    warn!(pubkey = %update.pubkey, error = ?err, "Failed to admit resolved-fetch account to primary subscription tier");
                                    subscription_tiers
                                        .clear_rejected_fetch_classification(
                                            &update.pubkey,
                                        )
                                        .await;
                                    classification_error =
                                        Some(err.to_string());
                                }
                            } else if apply_classification
                                && subscription_tiers
                                    .secondary
                                    .contains(&update.pubkey)
                            {
                                match subscription_tiers
                                    .try_promote_found_to_primary(
                                        update.pubkey,
                                        true,
                                    )
                                    .await
                                {
                                    Ok(PromotionOutcome::Promoted) => {}
                                    // The key was promoted by another
                                    // transition while this one was in
                                    // flight; it holds primary membership.
                                    Ok(PromotionOutcome::NotInSecondary) => {}
                                    // Evicted mid-promotion: the detached
                                    // eviction cleanup owns the follow-up;
                                    // the found update must not be forwarded
                                    // without primary membership.
                                    Ok(PromotionOutcome::Evicted) => {
                                        classification_error = Some(
                                            RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                                pubkey: update.pubkey,
                                            }
                                            .to_string(),
                                        );
                                    }
                                    Ok(PromotionOutcome::NoCapacity) => {
                                        let err = RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                            pubkey: update.pubkey,
                                        };
                                        subscription_tiers
                                            .finalize_rejected_promotion(
                                                &update.pubkey,
                                            )
                                            .await;
                                        classification_error =
                                            Some(err.to_string());
                                    }
                                    Err(err) => {
                                        warn!(pubkey = %update.pubkey, error = ?err, "Failed to promote found account to primary subscription tier");
                                        classification_error =
                                            Some(err.to_string());
                                    }
                                }
                            }
                            result
                        };

                    if let Some((_, waiters)) = resolved_fetch {
                        for sender in waiters {
                            let response = match classification_error.as_ref() {
                                Some(err) => Err(RemoteAccountProviderError::AccountResolutionsFailed(err.clone())),
                                None => Ok(remote_account.clone()),
                            };
                            let _ = sender.send(response);
                        }
                    }

                    if let Some(forward_update) = forward_update
                        .filter(|_| classification_error.is_none())
                    {
                        if let Err(err) =
                            subscription_forwarder.send(forward_update).await
                        {
                            warn!(
                                pubkey = %update.pubkey,
                                error = ?err,
                                "Failed to forward subscription update"
                            );
                        }
                    }
                }
            }
        });
        Ok(())
    }

    /// Convenience wrapper around [`RemoteAccountProvider::try_get_multi`] to fetch
    /// a single account.
    #[instrument(skip(self, fetch_context))]
    pub async fn try_get(
        &self,
        pubkey: Pubkey,
        fetch_context: impl Into<AccountFetchContext>,
    ) -> RemoteAccountProviderResult<RemoteAccount> {
        self.try_get_multi(&[pubkey], None, fetch_context, None)
            .await
            // SAFETY: we are guaranteed to have a single result here as
            // otherwise we would have gotten an error
            .map(|mut accs| accs.drain(..).next().unwrap())
    }

    #[instrument(skip(self, pubkeys, config, fetch_context))]
    pub async fn try_get_multi_until_slots_match(
        &self,
        pubkeys: &[Pubkey],
        config: Option<MatchSlotsConfig>,
        fetch_context: impl Into<AccountFetchContext>,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        use SlotsMatchResult::*;
        let fetch_context = fetch_context.into();
        let companion_fetch_kind =
            config.as_ref().map(|config| config.companion_fetch_kind);
        let config = config
            .as_ref()
            .map(MatchSlotsRetryConfig::from)
            .unwrap_or_default();
        let companion_fetch_started_at = std::time::Instant::now();
        let mut companion_fetch_attempts = 1u64;
        // 1. Fetch the _normal_ way and hope the slots match and if required
        //    the min_context_slot is met
        let mut remote_accounts = match self
            .try_get_multi(pubkeys, None, fetch_context.clone(), None)
            .await
        {
            Ok(accounts) => accounts,
            Err(err) => {
                observe_companion_fetch_if_configured(
                    fetch_context.clone(),
                    companion_fetch_kind,
                    ChainlinkCompanionFetchOutcome::FailedRpc,
                    companion_fetch_attempts,
                    companion_fetch_started_at,
                );
                return Err(err);
            }
        };
        // State observed at slot S must never be superseded by an older
        // view: raise the floor to any found result already consumed.
        let mut min_context_slot =
            raised_min_context_slot(config.min_context_slot, &remote_accounts);
        if let Match =
            slots_match_and_meet_min_context(&remote_accounts, min_context_slot)
        {
            observe_companion_fetch_if_configured(
                fetch_context.clone(),
                companion_fetch_kind,
                ChainlinkCompanionFetchOutcome::Succeeded,
                companion_fetch_attempts,
                companion_fetch_started_at,
            );
            return Ok(remote_accounts);
        }

        // Subscription results consumed to resolve this fetch must re-enter
        // the update pipeline if the fetch fails, or they are lost.
        let consumed_subscription_results: Vec<(Pubkey, RemoteAccount)> =
            pubkeys
                .iter()
                .zip(&remote_accounts)
                .filter(|(_, account)| {
                    account.is_found()
                        && account.source()
                            == Some(RemoteAccountUpdateSource::Subscription)
                })
                .map(|(pubkey, account)| (*pubkey, account.clone()))
                .collect();

        // The fetch start slot honors the strictest floor observed so far:
        // the caller's min_context_slot or any found result consumed above.
        let mut fetch_start_slot = self
            .chain_slot
            .load()
            .max(min_context_slot.unwrap_or_default());
        // 2. Wait for the slots to match. Once the fast path mixed slots,
        // retry with an RPC-only batch so all accounts share one response slot.
        let start = std::time::Instant::now();
        let mut retries = 0;
        loop {
            if tracing::enabled!(tracing::Level::TRACE) {
                let slots = account_slots(&remote_accounts);
                let pubkey_slots = pubkeys
                    .iter()
                    .zip(slots)
                    .map(|(pk, slot)| format!("{pk}:{slot}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                trace!(
                    "Retry({}) account fetch to sync non-matching slots [{}]",
                    retries + 1,
                    pubkey_slots
                );
            }
            companion_fetch_attempts += 1;
            remote_accounts = match self
                .fetch_multi_rpc_only(
                    pubkeys,
                    fetch_start_slot,
                    fetch_context.clone(),
                )
                .await
            {
                Ok(remote_accounts) => remote_accounts,
                Err(err) => {
                    let retry = next_match_slots_rpc_error_retry(
                        &mut retries,
                        start,
                        &config,
                    );
                    debug!(
                        pubkeys = %pubkeys_str(pubkeys),
                        min_context_slot = ?min_context_slot,
                        retries = retries,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        error = %err,
                        "RPC-only account fetch failed while resolving accounts to compatible slots"
                    );
                    match retry {
                        Ok(retry_delay) => {
                            tokio::time::sleep(retry_delay).await;
                            continue;
                        }
                        Err(_) => {
                            observe_companion_fetch_if_configured(
                                fetch_context,
                                companion_fetch_kind,
                                ChainlinkCompanionFetchOutcome::FailedRpc,
                                companion_fetch_attempts,
                                companion_fetch_started_at,
                            );
                            self.reforward_consumed_subscription_results(
                                &consumed_subscription_results,
                            );
                            return Err(err);
                        }
                    }
                }
            };
            for (pubkey, remote_account) in pubkeys.iter().zip(&remote_accounts)
            {
                let _subscription_guard =
                    subscription_key_owned_guard_from_map(
                        &self.subscription_key_locks,
                        *pubkey,
                    )
                    .await;
                if let Err(err) = self
                    .subscription_tier_ctx()
                    .apply_fetch_classification(
                        pubkey,
                        remote_account.slot(),
                        !remote_account.is_found(),
                    )
                    .await
                {
                    self.reforward_consumed_subscription_results(
                        &consumed_subscription_results,
                    );
                    return Err(err);
                }
            }
            min_context_slot =
                raised_min_context_slot(min_context_slot, &remote_accounts);
            fetch_start_slot =
                fetch_start_slot.max(min_context_slot.unwrap_or_default());
            let slots_match_result = slots_match_and_meet_min_context(
                &remote_accounts,
                min_context_slot,
            );
            if let Match = slots_match_result {
                observe_companion_fetch_if_configured(
                    fetch_context.clone(),
                    companion_fetch_kind,
                    ChainlinkCompanionFetchOutcome::Succeeded,
                    companion_fetch_attempts,
                    companion_fetch_started_at,
                );
                return Ok(remote_accounts);
            }

            match next_match_slots_retry(&mut retries, start, &config) {
                Ok(retry_delay) => {
                    // If the slots don't match then wait for a bit and retry
                    tokio::time::sleep(retry_delay).await;
                    continue;
                }
                Err(limit) => {
                    let remote_account_slots = account_slots(&remote_accounts);
                    let remote_account_sources = remote_accounts
                        .iter()
                        .map(|account| account.source())
                        .collect::<Vec<_>>();
                    warn!(
                        pubkeys = %pubkeys_str(pubkeys),
                        slots = ?remote_account_slots,
                        sources = ?remote_account_sources,
                        min_context_slot = ?min_context_slot,
                        retries = retries,
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        limit = %limit,
                        "Failed to resolve accounts to compatible slots"
                    );
                    match slots_match_result {
                        // SAFETY: Match case is already handled and returns
                        Match => unreachable!("we would have returned above"),
                        Mismatch => {
                            observe_companion_fetch_if_configured(
                                fetch_context,
                                companion_fetch_kind,
                                ChainlinkCompanionFetchOutcome::FailedSlotMismatch,
                                companion_fetch_attempts,
                                companion_fetch_started_at,
                            );
                            self.reforward_consumed_subscription_results(
                                &consumed_subscription_results,
                            );
                            return Err(
                                RemoteAccountProviderError::SlotsDidNotMatch(
                                    pubkeys_str(pubkeys),
                                    remote_account_slots,
                                    limit,
                                ),
                            );
                        }
                        MatchButBelowMinContextSlot(slot) => {
                            observe_companion_fetch_if_configured(
                                fetch_context,
                                companion_fetch_kind,
                                ChainlinkCompanionFetchOutcome::FailedMinContextSlot,
                                companion_fetch_attempts,
                                companion_fetch_started_at,
                            );
                            self.reforward_consumed_subscription_results(
                                &consumed_subscription_results,
                            );
                            return Err(RemoteAccountProviderError::MatchingSlotsNotSatisfyingMinContextSlot(
                                pubkeys_str(pubkeys),
                                remote_account_slots,
                                slot,
                                limit,
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Re-forwards found results consumed from the update pipeline by a
    /// fetch that is now failing; otherwise the consumed update is lost.
    /// Coalesced per account (newest slot wins) into an outbox drained by
    /// [Self::start_replay_outbox_worker], so callers never block and the
    /// backlog is bounded by the number of affected accounts.
    pub(super) fn reforward_consumed_subscription_results(
        &self,
        consumed: &[(Pubkey, RemoteAccount)],
    ) {
        if consumed.is_empty() {
            return;
        }
        {
            let mut outbox = self
                .replay_outbox
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for (pubkey, account) in consumed {
                let entry = outbox.entry(*pubkey);
                match entry {
                    Entry::Occupied(mut existing)
                        if existing.get().account.slot() < account.slot() =>
                    {
                        existing.insert(ForwardedSubscriptionUpdate {
                            pubkey: *pubkey,
                            account: account.clone(),
                            source: SubscriptionSource::Replay,
                        });
                    }
                    Entry::Occupied(_) => {}
                    Entry::Vacant(vacant) => {
                        vacant.insert(ForwardedSubscriptionUpdate {
                            pubkey: *pubkey,
                            account: account.clone(),
                            source: SubscriptionSource::Replay,
                        });
                    }
                }
            }
        }
        self.replay_notify.notify_one();
    }

    /// Drains the replay outbox into the update pipeline. Runs detached so
    /// replays never block a failing resolution; exits when the pipeline
    /// closes.
    pub(super) fn start_replay_outbox_worker(&self) {
        let outbox = Arc::clone(&self.replay_outbox);
        let notify = Arc::clone(&self.replay_notify);
        let forwarder = Arc::clone(&self.subscription_forwarder);
        task::spawn(async move {
            loop {
                notify.notified().await;
                loop {
                    let update = {
                        let mut outbox = outbox
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        let Some(pubkey) = outbox.keys().next().copied() else {
                            break;
                        };
                        outbox.remove(&pubkey)
                    };
                    let Some(update) = update else {
                        break;
                    };
                    if forwarder.send(update).await.is_err() {
                        return;
                    }
                }
            }
        });
    }
}
