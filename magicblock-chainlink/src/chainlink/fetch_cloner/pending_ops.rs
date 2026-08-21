use super::*;

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    /// Fetches and clones accounts while the engine coordinates concurrent
    /// requests for the same missing account.
    #[instrument(skip(self, pubkeys))]
    pub async fn fetch_and_clone_accounts_with_dedup(
        &self,
        pubkeys: &[Pubkey],
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        self.fetch_and_clone_accounts_with_dedup_forced_refresh(
            pubkeys,
            Some(pubkeys),
            slot,
            fetch_context,
            &HashSet::new(),
            None,
        )
        .await
    }

    pub(super) async fn fetch_and_clone_accounts_with_dedup_forced_refresh(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
        force_refresh_pubkeys: &HashSet<Pubkey>,
        confirmed_redelegation: Option<(Pubkey, u64)>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let mut pubkeys = pubkeys.iter().collect::<Vec<_>>();
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching and cloning accounts with dedup");
        }

        let requested_pubkeys =
            pubkeys.iter().map(|pubkey| **pubkey).collect::<Vec<_>>();
        let mut loads = Vec::new();
        let mut load_pubkeys = HashSet::new();
        let mut waiters = Vec::new();
        for missing in self.engine.accounts().ensure(&requested_pubkeys) {
            match missing {
                MissingAccount::Load(load) => {
                    load_pubkeys.insert(load.pubkey);
                    loads.push(load);
                }
                MissingAccount::Wait(wait) => waiters.push(wait),
            }
        }

        let mut in_bank = HashSet::new();
        let mut refresh = HashSet::new();
        let mut extra_mark_empty = Vec::new();
        let mut bank_hit_no_fetch_non_undelegating_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_still_valid_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_timeout_count = 0_u64;
        let mut bank_hit_undelegating_refresh_required_count = 0_u64;
        let mut bank_miss_remote_required_count = 0_u64;
        let mut forced_refresh_remote_required_count = 0_u64;

        // Phase 1: Sync bank check — separate undelegating accounts
        // (which need async RPC) from non-undelegating (handled
        // synchronously)
        let mut undelegating_checks = vec![];
        {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            for pubkey in pubkeys.iter() {
                if force_refresh_pubkeys.contains(*pubkey) {
                    if let Some(slot) = loader
                        .read(pubkey, |account| {
                            account
                                .is(AccountMode::Transient)
                                .then(|| account.slot())
                        })
                        .ok()
                        .flatten()
                        .flatten()
                    {
                        let can_replace = confirmed_redelegation.is_some_and(
                            |(confirmed_pubkey, delegation_slot)| {
                                confirmed_pubkey == **pubkey
                                    && delegation_slot > slot
                            },
                        );
                        if !can_replace {
                            bank_hit_no_fetch_undelegating_still_valid_count +=
                                1;
                            in_bank.insert(**pubkey);
                            continue;
                        }
                    }
                    forced_refresh_remote_required_count += 1;
                    refresh.insert(**pubkey);
                    continue;
                }
                let reader = |account: &AccountSharedData| {
                    if account.is(AccountMode::Transient) {
                        Err((
                            account.slot(),
                            account.is(AccountMode::Delegated),
                            ata_projection::derive_eata_pubkey_from_ata_layout(
                                pubkey,
                                account.data(),
                            ),
                        ))
                    } else {
                        Ok((
                            account.owner().eq(&dlp_api::id()),
                            account.is(AccountMode::Delegated),
                            *account.owner(),
                        ))
                    }
                };
                if let Some(account_in_bank) =
                    loader.read(pubkey, reader).ok().flatten()
                {
                    match account_in_bank {
                        Err(account_in_bank) => {
                            undelegating_checks
                                .push((**pubkey, account_in_bank));
                        }
                        Ok((owned_by_dlp, delegated, owner)) => {
                            if owned_by_dlp {
                                debug!(
                                    pubkey = %pubkey,
                                    "Account owned by deleg program not marked as undelegating"
                                );
                            }
                            if tracing::enabled!(tracing::Level::TRACE) {
                                trace!(
                                    pubkey = %pubkey,
                                    undelegating = false,
                                    delegated,
                                    owner = %owner,
                                    "Account found in bank in valid state, no fetch needed"
                                );
                            }
                            bank_hit_no_fetch_non_undelegating_count += 1;
                            in_bank.insert(**pubkey);
                        }
                    }
                } else {
                    bank_miss_remote_required_count += 1;
                }
            }
        }

        // Phase 2: Parallel undelegation checks via JoinSet
        if !undelegating_checks.is_empty() {
            let mut join_set = JoinSet::new();
            for (pubkey, (slot, delegated, eata_pubkey)) in undelegating_checks
            {
                let this = self.clone();
                let fetch_context = fetch_context.clone();
                join_set.spawn(async move {
                    let decision = match tokio::time::timeout(
                        Duration::from_secs(5),
                        this.should_refresh_undelegating_in_bank_account(
                            &pubkey,
                            slot,
                            delegated,
                            eata_pubkey,
                            fetch_context,
                        ),
                    )
                    .await
                    {
                        Ok(decision) => decision,
                        Err(_timeout) => {
                            warn!(
                                pubkey = %pubkey,
                                "Timeout checking if account is still undelegating after 5 seconds"
                            );
                            return (pubkey, None);
                        }
                    };
                    (pubkey, Some(decision))
                });
            }

            for (pubkey, decision) in join_set.join_all().await {
                match decision {
                    Some(
                        decision @ (RefreshDecision::Yes
                        | RefreshDecision::YesAndMarkEmptyIfNotFound),
                    ) => {
                        debug!(
                            pubkey = %pubkey,
                            "Account completed undelegation which was missed and is fetched again"
                        );
                        bank_hit_undelegating_refresh_required_count += 1;
                        refresh.insert(pubkey);
                        metrics::inc_unstuck_undelegation_count();
                        if let RefreshDecision::YesAndMarkEmptyIfNotFound =
                            decision
                        {
                            extra_mark_empty.push(pubkey);
                        }
                    }
                    Some(RefreshDecision::No) => {
                        if tracing::enabled!(tracing::Level::TRACE) {
                            trace!(
                                pubkey = %pubkey,
                                "Undelegating account still valid, no fetch needed"
                            );
                        }
                        bank_hit_no_fetch_undelegating_still_valid_count += 1;
                        in_bank.insert(pubkey);
                    }
                    None => {
                        bank_hit_no_fetch_undelegating_timeout_count += 1;
                        in_bank.insert(pubkey);
                    }
                }
            }
        }
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::NonUndelegatingPresent,
            bank_hit_no_fetch_non_undelegating_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::UndelegatingStillValid,
            bank_hit_no_fetch_undelegating_still_valid_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::UndelegatingCheckTimeout,
            bank_hit_no_fetch_undelegating_timeout_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context
                .clone()
                .with_reason(AccountFetchReason::UndelegatingRefresh),
            BankPrecheckOutcome::BankHitUndelegatingRefreshRequired,
            BankPrecheckReason::UndelegatingRefresh,
            bank_hit_undelegating_refresh_required_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankMissRemoteRequired,
            BankPrecheckReason::Absent,
            bank_miss_remote_required_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::ForcedRefreshRemoteRequired,
            BankPrecheckReason::ForcedRefresh,
            forced_refresh_remote_required_count,
        );
        pubkeys.retain(|p| !in_bank.contains(p));

        let mut mark_empty = mark_empty_if_not_found
            .unwrap_or(&[])
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        mark_empty.extend(extra_mark_empty);
        let mark_empty = mark_empty.into_iter().collect::<Vec<_>>();
        let mark_empty =
            (!mark_empty.is_empty()).then_some(mark_empty.as_slice());

        // Existing accounts that require a forced lifecycle refresh are not
        // returned by `ensure`; missing accounts are fetched only by the caller
        // holding the engine load reservation.
        let fetch_pubkeys = pubkeys
            .into_iter()
            .copied()
            .filter(|pubkey| {
                refresh.contains(pubkey) || load_pubkeys.contains(pubkey)
            })
            .collect::<Vec<_>>();
        let batch = if fetch_pubkeys.is_empty() {
            FetchAndCloneBatchResult::default()
        } else {
            self.fetch_and_clone_accounts(
                &fetch_pubkeys,
                mark_empty,
                slot,
                fetch_context,
            )
            .await?
        };

        let FetchAndCloneBatchResult {
            result,
            materialized,
        } = batch;
        let mut materialized = materialized
            .into_iter()
            .map(|account| (account.pubkey, account.mode))
            .collect::<HashMap<_, _>>();
        for load in loads {
            let Some(mode) = materialized.remove(&load.pubkey) else {
                continue;
            };
            load.complete(mode).await;
        }

        for waiter in waiters {
            let (pubkey, completed) = waiter.wait().await;
            if !completed {
                return Err(ChainlinkError::AccountLoadFailed(pubkey));
            }
        }

        Ok(result)
    }
}
