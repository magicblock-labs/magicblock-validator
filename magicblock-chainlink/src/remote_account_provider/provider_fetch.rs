//! RPC fetching: multi-account fetch, dedup, retries, task orchestration.

use std::collections::{hash_map::Entry, HashMap, HashSet};

pub(crate) use chain_pubsub_client::ChainPubsubClient;
pub(crate) use chain_rpc_client::ChainRpcClient;
pub(crate) use errors::{
    RemoteAccountProviderError, RemoteAccountProviderResult,
};
use magicblock_metrics::{
    metrics,
    metrics::{
        inc_account_fetches_failed, inc_account_fetches_found_with_context,
        inc_account_fetches_not_found_with_context,
        inc_account_fetches_success,
        inc_chainlink_empty_placeholder_accounts_total_with_context,
        inc_chainlink_pending_fetch_accounts_with_context,
        inc_chainlink_pending_fetch_waiters_gauge,
        inc_chainlink_pending_fetch_waiters_with_context,
        observe_chainlink_pending_fetch_owner_duration_seconds_with_context,
        AccountFetchContext, ChainlinkEmptyPlaceholderStage,
        ChainlinkPendingFetchLayer, ChainlinkPendingFetchOutcome, Outcome,
    },
};
pub(crate) use remote_account::RemoteAccount;
use solana_account::Account;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    client_error::ErrorKind, config::RpcAccountInfoConfig,
    custom_error::JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED,
    request::RpcError,
};
use tokio::sync::oneshot;
use tracing::*;

use super::*;

impl<T: ChainRpcClient, U: ChainPubsubClient> RemoteAccountProvider<T, U> {
    /// Gets the accounts for the given pubkeys by fetching from RPC.
    /// Always fetches fresh data. FetchCloner handles request deduplication.
    /// Subscribes first to catch any updates that arrive during fetch.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found, fetch_context))]
    pub async fn try_get_multi(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_context: impl Into<AccountFetchContext>,
        fetch_start_slot: Option<u64>,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        if pubkeys.is_empty() {
            return Ok(vec![]);
        }
        let fetch_context = fetch_context.into();

        if tracing::enabled!(tracing::Level::TRACE) {
            trace!("Fetching accounts");
        }

        let fetch_start_slot =
            fetch_start_slot.unwrap_or_else(|| self.chain_slot.load());

        // Receivers awaited by this call. One entry per input pubkey, in
        // input order. Each receiver corresponds to a sender that was
        // pushed into the FetchingAccountState.waiters queue (either
        // because this call inserted the entry, or because it joined an
        // existing in-flight fetch as a waiter).
        let mut await_receivers: Vec<(
            Pubkey,
            oneshot::Receiver<FetchResult>,
            PendingFetchWaiterGaugeGuard,
        )> = Vec::with_capacity(pubkeys.len());

        // Pubkeys this call actually inserted.
        // Only these pubkeys cause side effects (subscription setup, fetch)
        // by this call. Waiter-only pubkeys already have a subscription owned
        // by the original claimer and are being fetched by them.
        let mut claimed_pubkeys: Vec<Pubkey> = Vec::new();
        let mut claimed_generations: HashMap<
            Pubkey,
            FetchingAccountGeneration,
        > = HashMap::new();

        {
            let mut fetching = self
                .fetching_accounts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for &pubkey in pubkeys {
                let (sender, receiver) = oneshot::channel();
                let mut claimed = false;
                let layer = ChainlinkPendingFetchLayer::RemoteAccountProvider;
                let mut waiter_guard =
                    PendingFetchWaiterGaugeGuard::inactive(layer);
                match fetching.entry(pubkey) {
                    Entry::Occupied(mut entry) => {
                        entry.get_mut().waiters.push(sender);
                        inc_chainlink_pending_fetch_accounts_with_context(
                            fetch_context.clone(),
                            layer,
                            ChainlinkPendingFetchOutcome::JoinedExisting,
                            1,
                        );
                        inc_chainlink_pending_fetch_waiters_with_context(
                            fetch_context.clone(),
                            layer,
                            1,
                        );
                        inc_chainlink_pending_fetch_waiters_gauge(layer);
                        waiter_guard =
                            PendingFetchWaiterGaugeGuard::active(layer);
                    }
                    Entry::Vacant(entry) => {
                        let generation =
                            self.next_fetching_account_generation();
                        entry.insert(FetchingAccountState {
                            generation,
                            fetch_start_slot,
                            fetch_context: fetch_context.clone(),
                            owner_started_at: std::time::Instant::now(),
                            waiters: vec![sender],
                        });
                        inc_chainlink_pending_fetch_accounts_with_context(
                            fetch_context.clone(),
                            layer,
                            ChainlinkPendingFetchOutcome::Owned,
                            1,
                        );
                        claimed_generations.insert(pubkey, generation);
                        claimed = true;
                    }
                }
                if claimed {
                    claimed_pubkeys.push(pubkey);
                }
                await_receivers.push((pubkey, receiver, waiter_guard));
            }
        }

        if fetch_context.should_count_remote_account_claims()
            && !claimed_pubkeys.is_empty()
        {
            let unique_claimed_pubkey_count = claimed_pubkeys
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len();
            fetch_context
                .add_remote_account_claims(unique_claimed_pubkey_count);
        }

        // Setup subscriptions and trigger the fetch only for pubkeys this
        // call actually claimed. Waiter-only pubkeys already have a
        // subscription and an in-flight fetch owned by the original
        // claimer; doing it again here would duplicate work and (for
        // setup_subscriptions) double-count subscription side effects.
        if !claimed_pubkeys.is_empty() {
            let mut subscription_setup_guard =
                ClaimedSubscriptionSetupGuard::new(
                    self.fetching_accounts.clone(),
                    self.subscription_ownership.clone(),
                    self.subscription_transition_lock.clone(),
                    self.lrucache_subscribed_accounts.clone(),
                    self.secondary_subscriptions.clone(),
                    claimed_pubkeys.clone(),
                    claimed_generations.clone(),
                );
            if let Err(err) = self
                .setup_subscriptions(&claimed_pubkeys, fetch_context.clone())
                .await
            {
                subscription_setup_guard
                    .cleanup_with_error(err.to_string())
                    .await;
                return Err(err);
            }
            subscription_setup_guard.disarm();

            // Start the fetch for the claimed pubkeys only. Claim sets
            // above the RPC limit are split into evenly sized chunks
            let min_context_slot = fetch_start_slot;
            for chunk in balanced_chunks(claimed_pubkeys) {
                self.fetch(
                    chunk,
                    claimed_generations.clone(),
                    mark_empty_if_not_found,
                    min_context_slot,
                    fetch_context.clone(),
                );
            }
        }

        // Wait for all accounts to resolve (either from fetch or
        // subscription override). We await receivers in input pubkey
        // order so the returned Vec is index-aligned with `pubkeys`.
        let mut resolved_accounts = vec![];
        let mut errors = vec![];

        for (idx, (pubkey, receiver, mut waiter_guard)) in
            await_receivers.into_iter().enumerate()
        {
            let receiver_result = receiver.await;
            waiter_guard.finish();
            match receiver_result {
                Ok(result) => match result {
                    Ok(remote_account) => {
                        resolved_accounts.push(remote_account)
                    }
                    Err(err) => {
                        warn!(pubkey = %pubkey, error = %err, "Failed to fetch account");
                        errors.push((idx, err));
                    }
                },
                Err(err) => {
                    warn!(pubkey = %pubkey, stream_index = idx, error = ?err, total_pubkeys = pubkeys.len(), "Failed to resolve account (unexpected RecvError)");
                    errors.push((
                        idx,
                        RemoteAccountProviderError::RecvrError(err),
                    ));
                }
            }
        }

        if errors.is_empty() {
            assert_eq!(
                resolved_accounts.len(),
                pubkeys.len(),
                "BUG: resolved accounts and pubkeys length mismatch"
            );
            Ok(resolved_accounts)
        } else {
            Err(RemoteAccountProviderError::AccountResolutionsFailed(
                errors
                    .iter()
                    .map(|(idx, err)| {
                        let pubkey = pubkeys
                            .get(*idx)
                            .map(|pk| pk.to_string())
                            .unwrap_or_else(|| {
                                "BUG: could not match pubkey".to_string()
                            });
                        format!("{pubkey}: {err:?}")
                    })
                    .collect::<Vec<_>>()
                    .join(",\n"),
            ))
        }
    }

    pub(super) async fn fetch_multi_rpc_only(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        fetch_context: AccountFetchContext,
    ) -> RemoteAccountProviderResult<Vec<RemoteAccount>> {
        // This must stay a single wire call so all results share one
        // response slot (the slot-match contract callers verify);
        // slot-consistent sets must fit within the RPC limit.
        debug_assert!(
            pubkeys.len() <= MAX_MULTIPLE_ACCOUNTS_PER_REQUEST,
            "fetch_multi_rpc_only cannot chunk {} keys (limit {})",
            pubkeys.len(),
            MAX_MULTIPLE_ACCOUNTS_PER_REQUEST
        );
        let config = RpcAccountInfoConfig {
            commitment: Some(self.rpc_client.commitment()),
            min_context_slot: Some(min_context_slot),
            encoding: Some(UiAccountEncoding::Base64Zstd),
            data_slice: None,
        };

        metrics::inc_remote_account_provider_a_count();
        let response = tokio::time::timeout(RPC_FETCH_TIMEOUT, async {
            self.rpc_client
                .get_multiple_accounts_with_config(pubkeys, config)
                .await
        })
        .await
        .map_err(|_| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RPC call timeout fetching accounts {} after {}ms",
                pubkeys_str(pubkeys),
                RPC_FETCH_TIMEOUT.as_millis()
            ))
        })?
        .map_err(|err| {
            RemoteAccountProviderError::AccountResolutionsFailed(format!(
                "RpcError fetching accounts {}: {err:?}",
                pubkeys_str(pubkeys)
            ))
        })?;

        let response_slot = response.context.slot;
        if response_slot < min_context_slot {
            return Err(RemoteAccountProviderError::AccountResolutionsFailed(
                format!(
                    "Response slot {response_slot} < {min_context_slot} fetching accounts {}",
                    pubkeys_str(pubkeys)
                ),
            ));
        }

        let response_value_len = response.value.len();
        if response_value_len != pubkeys.len() {
            return Err(RemoteAccountProviderError::AccountResolutionsFailed(
                format!(
                    "RPC returned {response_value_len} account results for {} requested accounts: {}",
                    pubkeys.len(),
                    pubkeys_str(pubkeys)
                ),
            ));
        }

        let mut found_count = 0u64;
        let mut not_found_count = 0u64;
        let remote_accounts = response
            .value
            .into_iter()
            .map(|account| match account {
                Some(account) => {
                    found_count += 1;
                    RemoteAccount::from_fresh_account(
                        account,
                        response_slot,
                        RemoteAccountUpdateSource::Fetch,
                    )
                }
                None => {
                    not_found_count += 1;
                    RemoteAccount::NotFound(response_slot)
                }
            })
            .collect();

        inc_account_fetches_success(pubkeys.len() as u64);
        inc_account_fetches_found_with_context(
            fetch_context.clone(),
            found_count,
        );
        inc_account_fetches_not_found_with_context(
            fetch_context,
            not_found_count,
        );

        Ok(remote_accounts)
    }

    /// Tries to fetch the given accounts from RPC.
    /// NOTE: if we get an RPC error we just log it and give up since there is no
    ///       obvious way how to handle this even if we were to bubble the error up.
    /// Any action that depends on those accounts to be there will fail.
    /// NOTE: this is not used during subscription updates since we receive the data
    ///       as part of that update, thus we won't have stale data issues.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fetch(
        &self,
        pubkeys: Vec<Pubkey>,
        generations: HashMap<Pubkey, FetchingAccountGeneration>,
        mark_empty_if_not_found: Option<&[Pubkey]>,
        min_context_slot: u64,
        fetch_context: AccountFetchContext,
    ) {
        let rpc_client = self.rpc_client.clone();
        let fetching_accounts = self.fetching_accounts.clone();
        let commitment = self.rpc_client.commitment();
        let mark_empty_if_not_found =
            mark_empty_if_not_found.unwrap_or(&[]).to_vec();
        let subscription_tiers = self.subscription_tier_ctx();
        tokio::spawn(async move {
            use RemoteAccount::*;

            let fetch_started_at = std::time::Instant::now();
            // Helper to notify all pending requests of fetch failure
            let notify_error = |error_msg: &str| {
                let mut fetching = fetching_accounts
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                warn!(
                    pubkey_count = pubkeys.len(),
                    pubkeys = %pubkeys_str(&pubkeys),
                    min_context_slot = min_context_slot,
                    commitment = ?commitment,
                    fetch_entrypoint = %fetch_context.entrypoint(),
                    fetch_reason = %fetch_context.reason(),
                    elapsed_ms = fetch_started_at.elapsed().as_millis() as u64,
                    error = %error_msg,
                    "{error_msg}"
                );
                inc_account_fetches_failed(pubkeys.len() as u64);

                for pubkey in &pubkeys {
                    // Update metrics
                    // Remove pending requests and send error
                    if let Some(generation) = generations.get(pubkey).copied() {
                        if let Some(state) =
                            remove_fetching_account_if_generation_matches(
                                &mut fetching,
                                pubkey,
                                generation,
                            )
                        {
                            observe_chainlink_pending_fetch_owner_duration_seconds_with_context(
                                state.fetch_context,
                                ChainlinkPendingFetchLayer::RemoteAccountProvider,
                                ChainlinkPendingFetchOutcome::OwnerFailed,
                                state.owner_started_at.elapsed().as_secs_f64(),
                            );
                            for sender in state.waiters {
                                let error = RemoteAccountProviderError::AccountResolutionsFailed(
                                    format!("{}: {}", pubkey, error_msg)
                                );
                                let _ = sender.send(Err(error));
                            }
                        }
                    }
                }
            };

            let mut remaining_retries: u64 = RPC_FETCH_MAX_RETRIES;

            if tracing::enabled!(tracing::Level::TRACE) {
                trace!(pubkeys = pubkeys_str(&pubkeys), "Fetching accounts");
            }

            macro_rules! retry {
                ($msg:expr) => {{
                    trace!($msg);
                    remaining_retries -= 1;
                    if remaining_retries <= 0 {
                        let err_msg = format!("Max retries {RPC_FETCH_MAX_RETRIES} reached, giving up on fetching accounts: {pubkeys:?}");
                        notify_error(&err_msg);
                        return;
                    }
                    tokio::time::sleep(RPC_FETCH_RETRY_DELAY).await;
                    continue;
                }};
            }
            let response = loop {
                // We provide the min_context slot in order to _force_ the RPC to update
                // its account cache. Otherwise we could just keep fetching the accounts
                // until the context slot is high enough.
                metrics::inc_remote_account_provider_a_count();
                match tokio::time::timeout(RPC_FETCH_TIMEOUT, async {
                    let config = RpcAccountInfoConfig {
                        commitment: Some(commitment),
                        min_context_slot: Some(min_context_slot),
                        encoding: Some(UiAccountEncoding::Base64Zstd),
                        data_slice: None,
                    };

                    if pubkeys.len() == 1 {
                        rpc_client
                            .get_account_with_config(&pubkeys[0], config)
                            .await
                            .map(|res| (res.context.slot, vec![res.value]))
                    } else {
                        rpc_client
                            .get_multiple_accounts_with_config(&pubkeys, config)
                            .await
                            .map(|res| (res.context.slot, res.value))
                    }
                })
                .await
                {
                    Ok(Ok(res)) => {
                        let (slot, value) = res;
                        if slot < min_context_slot {
                            retry!("Response slot {slot} < {min_context_slot}. Retrying...");
                        } else {
                            break (slot, value);
                        }
                    }
                    Ok(Err(err)) => match *err.kind {
                        ErrorKind::RpcError(rpc_err) => {
                            match rpc_err {
                                RpcError::ForUser(ref rpc_user_err) => {
                                    // When an account is not present for the desired
                                    // min-context slot then we normally get the below
                                    // handled `RpcResponseError`, but may also get the
                                    // following error from the RPC.
                                    // See test::ixtest_existing_account_for_future_slot
                                    // ```
                                    // RpcError(
                                    //   ForUser(
                                    //       "AccountNotFound: \
                                    // pubkey=DaeruQ4SukTQaJA5muyv51MQZok7oaCAF8fAW19mbJv5: \
                                    //        RPC response error -32016: \
                                    //        Minimum context slot has not been reached; ",
                                    //   ),
                                    // )
                                    // ```
                                    retry!("Fetching accounts failed: {rpc_user_err:?}");
                                 }
                                RpcError::RpcResponseError {
                                    code,
                                    message,
                                    data,
                                } => {
                                    if code == JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED || code == HELIUS_CONTEXT_SLOT_NOT_REACHED {
                                        retry!("Minimum context slot {min_context_slot} not reached for {commitment:?}. code={code}, message={message}, data={data:?}");
                                    } else {
                                        let err = RpcError::RpcResponseError {
                                            code,
                                            message,
                                            data,
                                        };
                                        let err_msg = format!(
                                            "RpcError fetching accounts {}: {err:?}", pubkeys_str(&pubkeys)
                                        );
                                        notify_error(&err_msg);
                                        return;
                                    }
                                }
                                err => {
                                    let err_msg = format!(
                                        "RpcError fetching accounts {}: {err:?}", pubkeys_str(&pubkeys)
                                    );
                                     notify_error(&err_msg);
                                     return;
                                 }
                            }
                        }
                        ErrorKind::Custom(message)
                            if message
                                .to_ascii_lowercase()
                                .contains("minimum context slot") =>
                        {
                            retry!(
                                "Minimum context slot {min_context_slot} not reached for {commitment:?}: {message}"
                            );
                        }
                        _ => {
                            let err_msg = format!(
                                "RpcError fetching accounts {}: {err:?}",
                                pubkeys_str(&pubkeys)
                            );
                            notify_error(&err_msg);
                            return;
                        }
                    },
                    Err(_) => {
                        let attempt =
                            RPC_FETCH_MAX_RETRIES - remaining_retries + 1;
                        warn!(
                                pubkey_count = pubkeys.len(),
                                pubkeys = %pubkeys_str(&pubkeys),
                                attempt = attempt,
                                max_retries = RPC_FETCH_MAX_RETRIES,
                                remaining_retries = remaining_retries.saturating_sub(1),
                                timeout_ms = RPC_FETCH_TIMEOUT.as_millis() as u64,
                                elapsed_ms = fetch_started_at.elapsed().as_millis() as u64,
                                min_context_slot = min_context_slot,
                                commitment = ?commitment,
                                fetch_entrypoint = %fetch_context.entrypoint(),
                        fetch_reason = %fetch_context.reason(),
                                "RPC call timeout. Retrying..."
                            );
                        remaining_retries -= 1;
                        if remaining_retries == 0 {
                            let err_msg = format!("Max retries {RPC_FETCH_MAX_RETRIES} reached, giving up on fetching accounts: {pubkeys:?}");
                            notify_error(&err_msg);
                            return;
                        }
                        tokio::time::sleep(RPC_FETCH_RETRY_DELAY).await;
                        continue;
                    }
                };
            };

            // TODO: should we retry if not or respond with an error?
            let (response_slot, response_value) = response;
            assert!(response_slot >= min_context_slot);

            if response_value.len() != pubkeys.len() {
                let err_msg = format!(
                    "RPC returned {} account results for {} requested accounts: {}",
                    response_value.len(),
                    pubkeys.len(),
                    pubkeys_str(&pubkeys)
                );
                notify_error(&err_msg);
                return;
            }

            let mut found_count = 0u64;
            let mut not_found_count = 0u64;
            let mut not_found_pubkeys = HashSet::new();

            let remote_accounts: Vec<RemoteAccount> = pubkeys
                .iter()
                .zip(response_value)
                .map(|(pubkey, acc)| match acc {
                    Some(value) => {
                        found_count += 1;
                        RemoteAccount::from_fresh_account(
                            value,
                            response_slot,
                            RemoteAccountUpdateSource::Fetch,
                        )
                    }
                    None if mark_empty_if_not_found.contains(pubkey) => {
                        not_found_count += 1;
                        not_found_pubkeys.insert(*pubkey);
                        inc_chainlink_empty_placeholder_accounts_total_with_context(
                            fetch_context.clone(),
                            ChainlinkEmptyPlaceholderStage::ConvertedToEmpty,
                            Outcome::Success,
                        );
                        RemoteAccount::from_fresh_account(
                            Account {
                                lamports: 0,
                                data: vec![],
                                owner: Pubkey::default(),
                                executable: false,
                                rent_epoch: 0,
                            },
                            response_slot,
                            RemoteAccountUpdateSource::Fetch,
                        )
                    }
                    None => {
                        not_found_count += 1;
                        not_found_pubkeys.insert(*pubkey);
                        NotFound(response_slot)
                    }
                })
                .collect();

            // Update metrics for successful RPC fetch
            inc_account_fetches_success(pubkeys.len() as u64);
            inc_account_fetches_found_with_context(
                fetch_context.clone(),
                found_count,
            );
            inc_account_fetches_not_found_with_context(
                fetch_context.clone(),
                not_found_count,
            );

            if tracing::enabled!(tracing::Level::TRACE) {
                let pubkeys = pubkeys
                    .iter()
                    .map(|pk| pk.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                trace!(
                    pubkeys = %pubkeys, remote_accounts = ?remote_accounts, "Fetched, notifying pending requests"
                );
            }

            // Notify all pending requests with fetch results (unless subscription override occurred)
            for (pubkey, remote_account) in
                pubkeys.iter().zip(remote_accounts.iter())
            {
                let (waiters, classification_result) = {
                    // The per-key guard serializes this resolution against
                    // subscription updates and other transitions of the same
                    // key; the tier helpers scope the transition lock to
                    // their in-memory critical sections.
                    let _subscription_guard =
                        subscription_key_owned_guard_from_map(
                            &subscription_tiers.subscription_key_locks,
                            *pubkey,
                        )
                        .await;
                    // Remove from fetching and get pending requests
                    // Note: the account might have been resolved by a
                    // subscription update already or replaced by a newer owner.
                    let Some(generation) = generations.get(pubkey).copied()
                    else {
                        continue;
                    };
                    let state = {
                        let mut fetching = fetching_accounts
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        remove_fetching_account_if_generation_matches(
                            &mut fetching,
                            pubkey,
                            generation,
                        )
                    };
                    if let Some(state) = state {
                        let waiters = state.waiters;

                        let classification_result = subscription_tiers
                            .apply_fetch_classification(
                                pubkey,
                                response_slot,
                                not_found_pubkeys.contains(pubkey),
                            )
                            .await;
                        observe_chainlink_pending_fetch_owner_duration_seconds_with_context(
                            state.fetch_context.clone(),
                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                            if classification_result.is_ok() {
                                ChainlinkPendingFetchOutcome::OwnerSucceeded
                            } else {
                                ChainlinkPendingFetchOutcome::OwnerFailed
                            },
                            state.owner_started_at.elapsed().as_secs_f64(),
                        );
                        (waiters, classification_result)
                    } else {
                        inc_chainlink_pending_fetch_accounts_with_context(
                            fetch_context.clone(),
                            ChainlinkPendingFetchLayer::RemoteAccountProvider,
                            ChainlinkPendingFetchOutcome::RpcFetchCompletedAfterUpdate,
                            1,
                        );
                        if tracing::enabled!(tracing::Level::TRACE) {
                            trace!(
                                "Account {pubkey} generation {generation} was already resolved or replaced"
                            );
                        }
                        continue;
                    }
                };

                // Send the fetch result to all waiting requests
                for request in waiters {
                    let result = match &classification_result {
                        Ok(()) => Ok(remote_account.clone()),
                        Err(RemoteAccountProviderError::NoEvictableSubscriptionCapacity { pubkey }) => {
                            Err(RemoteAccountProviderError::NoEvictableSubscriptionCapacity {
                                pubkey: *pubkey,
                            })
                        }
                        Err(err) => Err(
                            RemoteAccountProviderError::AccountResolutionsFailed(
                                err.to_string(),
                            ),
                        ),
                    };
                    let _ = request.send(result);
                }
            }
        });
    }
}
