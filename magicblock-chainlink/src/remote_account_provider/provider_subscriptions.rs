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
        for (result, pubkey) in subscription_results.iter().zip(pubkeys.iter())
        {
            match result {
                Err(err) => {
                    error!(
                        pubkey = %pubkey, err = ?err,
                        "Failed to subscribe to account"
                    );
                    errors.push(format!("{}: {}", pubkey, err));
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
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!(
                        "{} subscription(s) failed: [{}]",
                        errors.len(),
                        errors.join(", ")
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
        // 1. First realize subscription
        if let Err(err) = self.pubsub_client.subscribe(*pubkey, None).await {
            inc_chainlink_subscription_registration_accounts(
                origin,
                reason.into(),
                SubscriptionRegistrationOutcome::SubscribeError,
            );
            return Err(err);
        }

        let outcome = if self.subscribed_accounts.add(*pubkey) {
            SubscriptionRegistrationOutcome::Added
        } else {
            SubscriptionRegistrationOutcome::AlreadyPresent
        };
        inc_chainlink_subscription_registration_accounts(
            origin,
            reason.into(),
            outcome,
        );

        Ok(())
    }

    /// Check if an account is currently being watched (subscribed to)
    /// This does not consider accounts like the clock sysvar that are watched as
    /// part of the provider's internal logic.
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.subscribed_accounts.contains(pubkey)
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
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;

        let mut ownership = self.subscription_ownership.lock().await;
        if let Some(existing) = ownership.get_mut(pubkey) {
            if !skip_existing_reason || !existing.contains(reason) {
                existing.acquire(reason);
            }
            self.subscribed_accounts.add(*pubkey);
            inc_chainlink_subscription_registration_accounts(
                origin,
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

        if self.subscribed_accounts.is_internally_managed(pubkey) {
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
            ownership.remove(pubkey);
            released_count
        };

        let success = subscription_reconciler::unsubscribe_account(
            *pubkey,
            &self.pubsub_client,
            SubscriptionCleanupSource::NormalRelease,
        )
        .await;

        if success {
            inc_chainlink_subscription_release_accounts(
                reason.into(),
                SubscriptionReleaseOutcome::Unsubscribed,
            );
            self.subscribed_accounts.remove(pubkey);
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
}

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
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
                     kept protected/live subscription and subscription-set entry"
                );
                return Ok(false);
            }

            ownership.remove(pubkey);
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
                self.subscribed_accounts.remove(pubkey);
                trace!(
                    pubkey = %pubkey,
                    ?reason,
                    "Removed final delegated-account subscription ownership \
                     and subscription-set entry silently; no removal notification emitted"
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
                    self.subscribed_accounts.remove(pubkey);
                    trace!(
                        pubkey = %pubkey,
                        ?reason,
                        "Removed stale delegated-account subscription-set entry for missing subscription"
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

        if self.subscribed_accounts.is_internally_managed(pubkey) {
            warn!(pubkey = %pubkey, "Tried to unsubscribe an internally managed account");
            inc_chainlink_subscription_cleanup_accounts(
                SubscriptionCleanupSource::ManualUnsubscribe,
                SubscriptionCleanupOutcome::RetainedIntentionally,
            );
            return Ok(());
        }

        if !self.subscribed_accounts.contains(pubkey) {
            trace!(pubkey = %pubkey, "Already unsubscribed from pubsub tracking");
            inc_chainlink_subscription_cleanup_accounts(
                SubscriptionCleanupSource::ManualUnsubscribe,
                SubscriptionCleanupOutcome::AlreadyAbsent,
            );
            return Ok(());
        }

        match self.pubsub_client.unsubscribe(*pubkey).await {
            Ok(()) => {
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::ManualUnsubscribe,
                    SubscriptionCleanupOutcome::Unsubscribed,
                );
            }
            Err(
                RemoteAccountProviderError::AccountSubscriptionDoesNotExist(_),
            ) => {
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::ManualUnsubscribe,
                    SubscriptionCleanupOutcome::AlreadyAbsent,
                );
            }
            Err(err) => {
                inc_chainlink_subscription_cleanup_accounts(
                    SubscriptionCleanupSource::ManualUnsubscribe,
                    SubscriptionCleanupOutcome::UnsubscribeFailed,
                );
                return Err(err);
            }
        }

        self.subscribed_accounts.remove(pubkey);
        self.subscription_ownership.lock().await.remove(pubkey);
        Ok(())
    }

    /// Releases a reason without restoring ownership after an unsubscribe
    /// failure. This is used when a bounded owner has already discarded its
    /// entry and must not leave an orphaned protected subscription behind.
    pub(crate) async fn forget_subscription_reason(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) {
        let subscription_key_lock = self.subscription_key_lock(pubkey).await;
        let _subscription_guard = subscription_key_lock.lock().await;
        let now_unowned = {
            let mut ownership = self.subscription_ownership.lock().await;
            let Some(existing) = ownership.get_mut(pubkey) else {
                return;
            };
            existing.release(reason);
            let empty = existing.is_empty();
            if empty {
                ownership.remove(pubkey);
            }
            empty
        };
        if now_unowned {
            let _ = subscription_reconciler::unsubscribe_account(
                *pubkey,
                &self.pubsub_client,
                SubscriptionCleanupSource::NormalRelease,
            )
            .await;
            self.subscribed_accounts.remove(pubkey);
        }
    }
}
