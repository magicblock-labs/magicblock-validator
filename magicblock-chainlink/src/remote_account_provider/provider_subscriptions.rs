//! Subscription lifecycle: tiers, acquire/ensure/release, program subs.

use std::sync::Arc;

pub(crate) use chain_pubsub_client::ChainPubsubClient;
pub(crate) use chain_rpc_client::ChainRpcClient;
pub(crate) use errors::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};
use futures_util::future::join_all;
use magicblock_metrics::metrics::{
    inc_chainlink_subscription_cleanup_accounts,
    inc_chainlink_subscription_registration_accounts,
    inc_chainlink_subscription_release_accounts, AccountFetchContext,
    SubscriptionCleanupOutcome, SubscriptionCleanupSource,
    SubscriptionRegistrationOrigin, SubscriptionRegistrationOutcome,
    SubscriptionReleaseOutcome,
};
use solana_account_decoder_client_types::{
    UiAccountEncoding, UiDataSliceConfig,
};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use tokio::sync::Mutex as AsyncMutex;
use tracing::*;

use super::*;

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    pub(super) async fn setup_subscriptions(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: AccountFetchContext,
    ) -> RemoteAccountProviderResult<()> {
        if tracing::enabled!(tracing::Level::TRACE) {
            let pubkeys_str = pubkeys
                .iter()
                .map(|pk| pk.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!(pubkeys = pubkeys_str, "Subscribing to accounts");
        }
        // Send all subscription requests in parallel (non-fail-fast).
        // We use join_all instead of try_join_all to ensure ALL acquire
        // attempts complete, even if some fail.
        let subscription_results = join_all(pubkeys.iter().map(|pubkey| {
            let fetch_context = fetch_context.clone();
            async move {
                self.acquire_subscription_with_origin(
                    pubkey,
                    SubscriptionReason::DirectAccount,
                    SubscriptionRegistrationOrigin::Fetch(fetch_context),
                )
                .await
            }
        }))
        .await;

        let mut errors = Vec::new();
        let mut acquired = Vec::new();
        for (result, pubkey) in
            subscription_results.into_iter().zip(pubkeys.iter())
        {
            match result {
                Err(err) => {
                    error!(
                        pubkey = %pubkey, err = ?err,
                        "Failed to subscribe to account"
                    );
                    errors.push((*pubkey, err));
                }
                Ok(()) => acquired.push(*pubkey),
            }
        }

        if !errors.is_empty() {
            for pubkey in &acquired {
                if let Err(unsub_err) = self
                    .release_single_subscription(
                        pubkey,
                        SubscriptionReason::DirectAccount,
                    )
                    .await
                {
                    if matches!(
                        unsub_err,
                        RemoteAccountProviderError::AccountSubscriptionDoesNotExist(_)
                    ) {
                        debug!(
                            pubkey = %pubkey, err = ?unsub_err,
                            "Failed to unsubscribe after partial \
                             subscription failure"
                        );
                    } else {
                        warn!(
                            pubkey = %pubkey, err = ?unsub_err,
                            "Failed to unsubscribe after partial \
                             subscription failure"
                        );
                    }
                }
            }
            // A single failure keeps its type so callers can react to
            // specific variants (e.g. capacity exhaustion).
            if errors.len() == 1 {
                // SAFETY: len checked above
                return Err(errors.pop().unwrap().1);
            }
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!(
                        "{} subscription(s) failed: [{}]",
                        errors.len(),
                        errors
                            .iter()
                            .map(|(pubkey, err)| format!("{pubkey}: {err}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
            );
        }

        Ok(())
    }

    /// Registers a new subscription for the given pubkey.
    pub(super) async fn register_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        origin: SubscriptionRegistrationOrigin,
    ) -> RemoteAccountProviderResult<()> {
        if matches!(origin, SubscriptionRegistrationOrigin::Fetch(_))
            && reason == SubscriptionReason::DirectAccount
            && self.lrucache_subscribed_accounts.can_evict(pubkey)
        {
            return self
                .subscription_tier_ctx()
                .register_secondary(pubkey, reason, origin)
                .await;
        }

        let tier_ctx = self.subscription_tier_ctx();
        let has_capacity = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            tier_ctx
                .has_capacity_with_protection(
                    &self.lrucache_subscribed_accounts,
                    pubkey,
                )
                .await
        };
        if !has_capacity {
            inc_chainlink_subscription_registration_accounts(
                origin,
                reason.into(),
                SubscriptionRegistrationOutcome::RejectedNoCapacity,
            );
            return Err(
                RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                    pubkey: *pubkey,
                },
            );
        }

        // 1. First realize subscription. Runs outside the transition lock;
        // the per-key guard held by the caller serializes this key.
        if let Err(err) = self.pubsub_client.subscribe(*pubkey, None).await {
            inc_chainlink_subscription_registration_accounts(
                origin,
                reason.into(),
                SubscriptionRegistrationOutcome::SubscribeError,
            );
            return Err(err);
        }

        // 2. Add to LRU cache
        // If an account is evicted then we need to unsubscribe from it
        // and then inform upstream that we are no longer tracking it
        let add_outcome = {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            let add_outcome = tier_ctx
                .add_with_protection(
                    &self.lrucache_subscribed_accounts,
                    *pubkey,
                )
                .await;
            if !matches!(add_outcome, AddAccountOutcome::NoEvictableCandidate) {
                self.remove_from_secondary(pubkey);
            }
            add_outcome
        };

        match add_outcome {
            AddAccountOutcome::AlreadyPresent => {
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::AlreadyPresent,
                );
            }
            AddAccountOutcome::Added => {
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::AddedBelowCapacity,
                );
            }
            AddAccountOutcome::Evicted(evicted) => {
                trace!(evicted = %evicted, "Evicting account");
                tier_ctx.spawn_evicted_cleanup(evicted);
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::EvictedCandidate,
                );
            }
            AddAccountOutcome::NoEvictableCandidate => {
                tier_ctx.cleanup_rejected_subscription(*pubkey).await?;
                debug!(
                    pubkey = %pubkey,
                    "No evictable subscription capacity available; all LRU candidates are protected"
                );
                inc_chainlink_subscription_registration_accounts(
                    origin,
                    reason.into(),
                    SubscriptionRegistrationOutcome::RejectedAndUnsubscribed,
                );
                return Err(
                    RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                        pubkey: *pubkey,
                    },
                );
            }
        }

        Ok(())
    }

    pub(crate) async fn send_removal_update(
        &self,
        evicted: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        self.removed_account_tx.send(evicted).await.map_err(
            RemoteAccountProviderError::FailedToSendAccountRemovalUpdate,
        )?;
        Ok(())
    }

    /// Check if an account is currently being watched (subscribed to)
    /// This does not consider accounts like the clock sysvar that are watched as
    /// part of the provider's internal logic.
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.lrucache_subscribed_accounts.contains(pubkey)
            || self.secondary_subscriptions.contains(pubkey)
    }

    /// Removes a pubkey from the secondary LRU; safe for never-evict keys.
    pub(super) fn remove_from_secondary(&self, pubkey: &Pubkey) {
        if self.secondary_subscriptions.contains(pubkey) {
            self.secondary_subscriptions.remove(pubkey);
        }
        self.confirmed_missing_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(pubkey);
    }

    pub(super) fn subscription_tier_ctx(&self) -> SubscriptionTierCtx<U> {
        SubscriptionTierCtx {
            primary: self.lrucache_subscribed_accounts.clone(),
            secondary: self.secondary_subscriptions.clone(),
            pubsub_client: self.pubsub_client.clone(),
            subscription_ownership: self.subscription_ownership.clone(),
            subscription_transition_lock: self
                .subscription_transition_lock
                .clone(),
            subscription_key_locks: self.subscription_key_locks.clone(),
            fetching_accounts: self.fetching_accounts.clone(),
            capacity_eviction_protection: self
                .capacity_eviction_protection
                .clone(),
            confirmed_missing_subscriptions: self
                .confirmed_missing_subscriptions
                .clone(),
            removed_account_tx: self.removed_account_tx.clone(),
        }
    }

    pub(crate) async fn evict_unwatched_with_subscription_lock<F, Fut>(
        &self,
        pubkey: &Pubkey,
        evict: F,
    ) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if self.is_watching(pubkey) {
            return false;
        }

        evict().await;
        true
    }

    pub(super) async fn subscription_key_lock(
        &self,
        pubkey: &Pubkey,
    ) -> Arc<AsyncMutex<()>> {
        subscription_key_lock_from_map(&self.subscription_key_locks, pubkey)
            .await
    }

    pub async fn acquire_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(
            pubkey,
            reason,
            false,
            SubscriptionRegistrationOrigin::Internal,
        )
        .await
    }

    pub(super) async fn acquire_subscription_with_origin(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        origin: SubscriptionRegistrationOrigin,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(pubkey, reason, false, origin)
            .await
    }

    pub async fn ensure_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<()> {
        self.acquire_subscription_with_mode(
            pubkey,
            reason,
            true,
            SubscriptionRegistrationOrigin::Internal,
        )
        .await
    }

    pub(super) async fn acquire_subscription_with_mode(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        skip_existing_reason: bool,
        origin: SubscriptionRegistrationOrigin,
    ) -> RemoteAccountProviderResult<()> {
        // The per-key guard serializes every transition of this key,
        // including the network calls made below. The transition lock is
        // acquired only inside the tier-state helpers.
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        let mut ownership = self.subscription_ownership.lock().await;
        if let Some(existing) = ownership.get_mut(pubkey) {
            let classification_placeholder_generation =
                existing.classification_placeholder_generation;
            let acquired_reason =
                !skip_existing_reason || !existing.contains(reason);
            if acquired_reason {
                existing.acquire(reason);
            }
            drop(ownership);

            let repair_result = if self
                .lrucache_subscribed_accounts
                .contains(pubkey)
            {
                self.lrucache_subscribed_accounts.promote_multi(&[pubkey]);
                Ok(())
            } else if self.secondary_subscriptions.contains(pubkey) {
                self.secondary_subscriptions.promote_multi(&[pubkey]);
                let keep_secondary =
                    matches!(origin, SubscriptionRegistrationOrigin::Fetch(_))
                        && reason == SubscriptionReason::DirectAccount;
                if !keep_secondary {
                    match self
                        .subscription_tier_ctx()
                        .try_promote_found_to_primary(*pubkey, true)
                        .await
                    {
                        Ok(PromotionOutcome::Promoted) => Ok(()),
                        // Promoted by another transition mid-flight; the key
                        // holds primary membership and the reason stands.
                        Ok(PromotionOutcome::NotInSecondary) => Ok(()),
                        // Evicted by another key's admission mid-flight;
                        // register it from scratch.
                        Ok(PromotionOutcome::Evicted) => {
                            self.register_subscription(pubkey, reason, origin.clone())
                                .await
                        }
                        Ok(PromotionOutcome::NoCapacity)
                            if reason
                                == SubscriptionReason::UndelegationTracking =>
                        {
                            Ok(())
                        }
                        Ok(PromotionOutcome::NoCapacity) => Err(
                            RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                pubkey: *pubkey,
                            },
                        ),
                        Err(err) => Err(err),
                    }
                } else {
                    let confirmed_missing = self
                        .confirmed_missing_subscriptions
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .contains(pubkey);
                    if confirmed_missing {
                        // Flow the error into repair_result (no early return)
                        // so the acquired-reason rollback below executes.
                        match self.pubsub_client.subscribe(*pubkey, None).await
                        {
                            Ok(()) => {
                                self.confirmed_missing_subscriptions
                                    .lock()
                                    .unwrap_or_else(|poison| {
                                        poison.into_inner()
                                    })
                                    .remove(pubkey);
                                Ok(())
                            }
                            Err(err) => Err(err),
                        }
                    } else {
                        Ok(())
                    }
                }
            } else {
                self.register_subscription(pubkey, reason, origin.clone())
                    .await
            };

            if let Err(err) = repair_result {
                if acquired_reason {
                    if let Some(existing) =
                        self.subscription_ownership.lock().await.get_mut(pubkey)
                    {
                        if existing.release(reason) {
                            existing.classification_placeholder_generation =
                                classification_placeholder_generation;
                        }
                    }
                }
                return Err(err);
            }
            inc_chainlink_subscription_registration_accounts(
                origin.clone(),
                reason.into(),
                SubscriptionRegistrationOutcome::AlreadyPresent,
            );
            return Ok(());
        }
        drop(ownership);

        self.register_subscription(pubkey, reason, origin.clone())
            .await?;

        let mut ownership = self.subscription_ownership.lock().await;
        ownership.entry(*pubkey).or_default().acquire(reason);
        Ok(())
    }

    pub async fn release_single_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<bool> {
        self.release_subscription_with_mode(
            pubkey,
            reason,
            SubscriptionReleaseMode::Single,
        )
        .await
    }

    pub(crate) async fn release_subscription_with_mode(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
        mode: SubscriptionReleaseMode,
    ) -> RemoteAccountProviderResult<bool> {
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if !self.lrucache_subscribed_accounts.can_evict(pubkey) {
            inc_chainlink_subscription_release_accounts(
                reason.into(),
                SubscriptionReleaseOutcome::RetainedIntentionally,
            );
            return Ok(false);
        }

        let released_count = {
            let mut ownership = self.subscription_ownership.lock().await;
            let (is_empty, released_count) = match ownership.get_mut(pubkey) {
                Some(existing) => {
                    let released_count = match mode {
                        SubscriptionReleaseMode::Single => {
                            existing.release(reason);
                            1
                        }
                        SubscriptionReleaseMode::All => {
                            existing.release_all(reason)
                        }
                    };
                    (existing.is_empty(), released_count)
                }
                None => {
                    inc_chainlink_subscription_release_accounts(
                        reason.into(),
                        SubscriptionReleaseOutcome::AlreadyAbsent,
                    );
                    return Ok(false);
                }
            };
            if !is_empty {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::RetainedOtherReasons,
                );
                return Ok(false);
            }
            released_count
        };

        let success = subscription_reconciler::unsubscribe_and_notify_removal(
            *pubkey,
            &self.pubsub_client,
            &self.removed_account_tx,
            SubscriptionCleanupSource::NormalRelease,
        )
        .await;

        if success {
            inc_chainlink_subscription_release_accounts(
                reason.into(),
                SubscriptionReleaseOutcome::Unsubscribed,
            );
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.subscription_ownership.lock().await.remove(pubkey);
            self.lrucache_subscribed_accounts.remove(pubkey);
            self.remove_from_secondary(pubkey);
        } else {
            inc_chainlink_subscription_release_accounts(
                reason.into(),
                SubscriptionReleaseOutcome::UnsubscribeFailed,
            );
            let mut ownership = self.subscription_ownership.lock().await;
            if ownership
                .get(pubkey)
                .is_none_or(SubscriptionOwnership::is_empty)
            {
                let ownership = ownership.entry(*pubkey).or_default();
                for _ in 0..released_count {
                    ownership.acquire(reason);
                }
            }
        }

        Ok(success)
    }

    pub(crate) async fn release_subscription_reason_silently_for_delegated_account(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> RemoteAccountProviderResult<bool> {
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        let released_count = {
            let release_mode = SubscriptionReleaseMode::All;
            let mut ownership = self.subscription_ownership.lock().await;
            let (is_empty, released_count) = match ownership.get_mut(pubkey) {
                Some(existing) => {
                    let released_count = match release_mode {
                        SubscriptionReleaseMode::Single => {
                            existing.release(reason);
                            1
                        }
                        SubscriptionReleaseMode::All => {
                            existing.release_all(reason)
                        }
                    };
                    (existing.is_empty(), released_count)
                }
                None => {
                    inc_chainlink_subscription_release_accounts(
                        reason.into(),
                        SubscriptionReleaseOutcome::AlreadyAbsent,
                    );
                    inc_chainlink_subscription_cleanup_accounts(
                        SubscriptionCleanupSource::DelegatedAccountSilent,
                        SubscriptionCleanupOutcome::AlreadyAbsent,
                    );
                    return Ok(false);
                }
            };

            if released_count == 0 {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::AlreadyAbsent,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::AlreadyAbsent,
                );
                return Ok(false);
            }

            if !is_empty {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::RetainedOtherReasons,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::RetainedIntentionally,
                );
                trace!(
                    pubkey = %pubkey,
                    ?reason,
                    released_count,
                    "Released delegated-account subscription ownership; \
                     kept protected/live subscription and LRU entry"
                );
                return Ok(false);
            }

            released_count
        };

        match self.pubsub_client.unsubscribe(*pubkey).await {
            Ok(()) => {
                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::Unsubscribed,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::Unsubscribed,
                );
                {
                    let _transition_guard =
                        self.subscription_transition_lock.lock().await;
                    self.subscription_ownership.lock().await.remove(pubkey);
                    self.lrucache_subscribed_accounts.remove(pubkey);
                    self.remove_from_secondary(pubkey);
                }
                trace!(
                    pubkey = %pubkey,
                    ?reason,
                    "Removed final delegated-account subscription ownership \
                     and LRU entry silently; no removal notification emitted"
                );
                Ok(true)
            }
            Err(err) => {
                if matches!(
                    err,
                    RemoteAccountProviderError::AccountSubscriptionDoesNotExist(
                        _
                    )
                ) {
                    inc_chainlink_subscription_release_accounts(
                        reason.into(),
                        SubscriptionReleaseOutcome::AlreadyAbsent,
                    );
                    inc_chainlink_subscription_cleanup_accounts(
                        SubscriptionCleanupSource::DelegatedAccountSilent,
                        SubscriptionCleanupOutcome::AlreadyAbsent,
                    );
                    {
                        let _transition_guard =
                            self.subscription_transition_lock.lock().await;
                        self.subscription_ownership.lock().await.remove(pubkey);
                        self.lrucache_subscribed_accounts.remove(pubkey);
                        self.remove_from_secondary(pubkey);
                    }
                    trace!(
                        pubkey = %pubkey,
                        ?reason,
                        "Removed stale delegated-account LRU entry for missing subscription"
                    );
                    return Ok(false);
                }

                inc_chainlink_subscription_release_accounts(
                    reason.into(),
                    SubscriptionReleaseOutcome::UnsubscribeFailed,
                );
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::DelegatedAccountSilent,
                    SubscriptionCleanupOutcome::UnsubscribeFailed,
                );
                let mut ownership = self.subscription_ownership.lock().await;
                if ownership
                    .get(pubkey)
                    .is_none_or(SubscriptionOwnership::is_empty)
                {
                    let ownership = ownership.entry(*pubkey).or_default();
                    for _ in 0..released_count {
                        ownership.acquire(reason);
                    }
                }
                drop(ownership);

                Err(err)
            }
        }
    }

    /// Subscribe to program account updates
    #[instrument(skip(self))]
    pub async fn subscribe_program(
        &self,
        program_id: Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        self.pubsub_client.subscribe_program(program_id).await
    }

    /// Unsubscribe from an account
    #[instrument(skip(self))]
    pub async fn unsubscribe(
        &self,
        pubkey: &Pubkey,
    ) -> RemoteAccountProviderResult<()> {
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        if !self.lrucache_subscribed_accounts.can_evict(pubkey) {
            warn!(pubkey = %pubkey, "Tried to unsubscribe from account that should never be evicted");
            inc_chainlink_subscription_cleanup_accounts(
                SubscriptionCleanupSource::ManualUnsubscribe,
                SubscriptionCleanupOutcome::RetainedIntentionally,
            );
            return Ok(());
        }

        if !self.lrucache_subscribed_accounts.contains(pubkey)
            && !self.secondary_subscriptions.contains(pubkey)
        {
            trace!(pubkey = %pubkey, "Already unsubscribed from LRU");
            inc_chainlink_subscription_cleanup_accounts(
                SubscriptionCleanupSource::ManualUnsubscribe,
                SubscriptionCleanupOutcome::AlreadyAbsent,
            );
            return Ok(());
        }

        let success = subscription_reconciler::unsubscribe_and_notify_removal(
            *pubkey,
            &self.pubsub_client,
            &self.removed_account_tx,
            SubscriptionCleanupSource::ManualUnsubscribe,
        )
        .await;

        if success {
            let _transition_guard =
                self.subscription_transition_lock.lock().await;
            self.lrucache_subscribed_accounts.remove(pubkey);
            self.remove_from_secondary(pubkey);
            self.subscription_ownership.lock().await.remove(pubkey);
        }

        Ok(())
    }

    /// Fetches a byte range of an account via `dataSlice`, retrying until the
    /// response context reaches `min_context_slot` so the caller never acts on
    /// pre-notification state. Returns `None` if the account does not exist.
    pub(crate) async fn get_account_data_slice(
        &self,
        pubkey: &Pubkey,
        offset: usize,
        length: usize,
        min_context_slot: u64,
    ) -> RemoteAccountProviderResult<Option<Vec<u8>>> {
        let mut last_err = String::new();
        for attempt in 1..=DATA_SLICE_FETCH_MAX_ATTEMPTS {
            let config = RpcAccountInfoConfig {
                commitment: Some(self.rpc_client.commitment()),
                // The context slot is verified locally below; passing
                // min_context_slot would surface provider-specific errors
                // instead of a plain lagging context.
                min_context_slot: None,
                encoding: Some(UiAccountEncoding::Base64),
                data_slice: Some(UiDataSliceConfig { offset, length }),
            };
            match tokio::time::timeout(
                RPC_FETCH_TIMEOUT,
                self.rpc_client.get_account_with_config(pubkey, config),
            )
            .await
            {
                Ok(Ok(response)) => {
                    if response.context.slot >= min_context_slot {
                        return Ok(response.value.map(|account| account.data));
                    }
                    last_err = format!(
                        "context slot {} below min {}",
                        response.context.slot, min_context_slot
                    );
                }
                Ok(Err(err)) => last_err = format!("{err:?}"),
                Err(_) => {
                    last_err = format!(
                        "timeout after {}ms",
                        RPC_FETCH_TIMEOUT.as_millis()
                    )
                }
            }
            if attempt < DATA_SLICE_FETCH_MAX_ATTEMPTS {
                tokio::time::sleep(RPC_FETCH_RETRY_DELAY).await;
            }
        }
        Err(RemoteAccountProviderError::AccountDataSliceFetchFailed(
            *pubkey,
            min_context_slot,
            last_err,
        ))
    }
}
