use super::*;

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

                    // Check if we're currently fetching this account
                    let forward_update = {
                        let mut fetching = fetching_accounts
                            .lock()
                            .expect("fetching_accounts lock poisoned");
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
                                        state.fetch_context,
                                        ChainlinkPendingFetchLayer::RemoteAccountProvider,
                                        ChainlinkPendingFetchOutcome::ResolvedBySubscriptionUpdate,
                                        1,
                                    );

                                    // Resolve all pending requests with the
                                    // subscription data and also forward it:
                                    // callers such as status reads may not
                                    // clone the result themselves.
                                    for sender in state.waiters {
                                        let _ = sender
                                            .send(Ok(remote_account.clone()));
                                    }
                                    Some(ForwardedSubscriptionUpdate {
                                        pubkey: update.pubkey,
                                        account: remote_account.clone(),
                                        source: update.source,
                                    })
                                } else {
                                    // Subscription is stale, put the fetch tracking back
                                    debug!(pubkey = %update.pubkey, slot = slot, fetch_start_slot = state.fetch_start_slot, generation, "Received stale subscription update");
                                    fetching.insert(update.pubkey, state);
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            Some(ForwardedSubscriptionUpdate {
                                pubkey: update.pubkey,
                                account: remote_account,
                                source: update.source,
                            })
                        }
                    };

                    if let Some(forward_update) = forward_update
                        && let Err(err) =
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
        });
        Ok(())
    }

    /// Re-forwards found results consumed by a fetch that is now failing.
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
