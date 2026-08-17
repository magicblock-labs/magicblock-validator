use super::*;

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    pub fn start_subscription_listener(
        self: Arc<Self>,
        mut subscription_updates: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) {
        tokio::spawn(async move {
            let semaphore = Arc::new(Semaphore::new(
                super::super::SUBSCRIPTION_UPDATE_LIMIT,
            ));
            let mut pending_tasks: JoinSet<()> = JoinSet::new();

            loop {
                while let Some(result) = pending_tasks.try_join_next() {
                    if let Err(err) = result {
                        warn!(error = ?err, "Subscription update task panicked");
                    }
                }

                // INVARIANT: The semaphore is created locally and never closed,
                // so acquire_owned() cannot fail with AcquireError.
                let permit = Arc::clone(&semaphore)
                    .acquire_owned()
                    .await
                    .expect("subscription update semaphore never closed");

                match subscription_updates.recv().await {
                    Some(update) => {
                        let pubkey = update.pubkey;
                        trace!(
                            pubkey = %pubkey,
                            "FetchCloner received subscription update"
                        );
                        let this = Arc::clone(&self);
                        metrics::inc_inflight_subscription_updates();
                        pending_tasks.spawn(async move {
                            struct InflightSubscriptionUpdateGuard;
                            impl Drop for InflightSubscriptionUpdateGuard {
                                fn drop(&mut self) {
                                    metrics::dec_inflight_subscription_updates(
                                    );
                                }
                            }
                            let _inflight_guard =
                                InflightSubscriptionUpdateGuard;

                            Self::process_subscription_update(
                                &this, pubkey, update,
                            )
                            .await;
                            this.processed_updates_count
                                .fetch_add(1, Ordering::Release);
                            drop(permit);
                        });
                    }
                    None => {
                        drop(permit);
                        while pending_tasks.join_next().await.is_some() {}
                        break;
                    }
                }
            }
        });
    }

    pub(super) async fn process_subscription_update(
        &self,
        pubkey: Pubkey,
        update: ForwardedSubscriptionUpdate,
    ) {
        let fresh_update_account = update.account.fresh_account();
        let is_dlp_owned_update = fresh_update_account
            .is_some_and(|account| account.owner() == &dlp_api::id());
        let is_internal_dlp_update =
            fresh_update_account.is_some_and(|account| {
                is_internal_dlp_account_data(account.data())
            });

        let dlp_program_interest =
            if matches!(update.source, SubscriptionSource::Program)
                && is_dlp_owned_update
            {
                match fresh_update_account {
                    Some(account) => Some(
                        self.classify_dlp_program_update_interest(
                            pubkey, account,
                        )
                        .await,
                    ),
                    None => None,
                }
            } else {
                None
            };

        match dlp_program_interest {
            Some(DlpProgramUpdateInterest::DropLocalDelegatedAuthoritative) => {
                self.cleanup_direct_subscription_for_delegated_account(pubkey)
                    .await;
                trace!(
                    pubkey = %pubkey,
                    "Dropping DLP program update for locally authoritative delegated account"
                );
                return;
            }
            Some(DlpProgramUpdateInterest::ProcessUndelegating)
            | Some(DlpProgramUpdateInterest::ProcessAtaProjection)
            | Some(DlpProgramUpdateInterest::ProcessDirectlyWatched)
            | Some(DlpProgramUpdateInterest::DiscoverDelegatedAccount)
            | None => {}
        }

        if !matches!(
            dlp_program_interest,
            Some(DlpProgramUpdateInterest::ProcessUndelegating)
                | Some(DlpProgramUpdateInterest::ProcessAtaProjection)
        ) && is_dlp_owned_update
            && is_internal_dlp_update
            && matches!(update.source, SubscriptionSource::Program)
        {
            self.resolve_internal_dlp_collision(pubkey, update.account.slot())
                .await;
            return;
        }

        if self
            .maybe_greedily_clone_discovered_delegated_account(pubkey, &update)
            .await
        {
            return;
        }
        // A late forwarded update can arrive after an account was removed from
        // the provider watch set. If a new subscription already won the race,
        // is_watching is true and this update can be processed normally. If this
        // update wins before acquire_subscription completes, the update is dropped;
        // the new subscription path performs its own fetch and clones fresh state.
        // If stale state is still present locally, cleanup is routed through the
        // existing removal listener, which serializes the final is_watching check and
        // eviction submission against same-pubkey subscription transitions.
        //
        // The guard only applies to account-subscription updates: the
        // account-sub pubsub tracking is the source of truth for `is_watching`. Program
        // subscription updates can legitimately arrive for pubkeys that are
        // *not* in the account-sub pubsub tracking (e.g. delegated accounts whose direct
        // subscription was released after cloning and are now tracked only via
        // their owner program). Dropping those would leave the bank stuck in a
        // stale delegated/undelegated state.
        let update_slot = update.account.slot();
        if matches!(update.source, SubscriptionSource::Account)
            && !self.remote_account_provider.is_watching(&pubkey)
        {
            trace!(
                pubkey = %pubkey,
                update_slot,
                "Dropping subscription update for account that is no longer watched"
            );
            if self.contains_account(&pubkey)
                && let Err(err) = self
                    .remote_account_provider
                    .send_stale_account(pubkey)
                    .await
            {
                warn!(
                    pubkey = %pubkey,
                    error = ?err,
                    "Failed to enqueue stale subscription update removal"
                );
            }
            return;
        }

        let companion_fetch_log_context = CompanionFetchLogContext {
            origin: AccountFetchContext::subscription_update(
                AccountFetchReason::SubscriptionUpdateClone,
            ),
            primary_pubkey: pubkey,
            context_slot: update_slot,
        };

        let routed_program_id =
            self.programdata_index.lock().get(&pubkey).copied();
        if let Some(program_id) = routed_program_id {
            let program_account =
                AccountBuilder::from(AccountSharedData::new(1, 0, &LOADER_V3))
                    .slot(update_slot);
            self.handle_executable_sub_update(
                program_id,
                program_account,
                &companion_fetch_log_context,
            )
            .await;
            return;
        }

        let update_source = update.source;
        let (resolved_account, deleg_record, delegation_actions) = self
            .resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
                update,
                &companion_fetch_log_context,
            )
            .await;
        let Some(account) = resolved_account else {
            return;
        };
        let subscription_clone_context =
            AccountFetchContext::subscription_update(
                AccountFetchReason::SubscriptionUpdateClone,
            );
        let projected_ata_clone_request = self
            .maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
                pubkey,
                &account,
                update_source,
                deleg_record.as_ref(),
                &delegation_actions,
                &companion_fetch_log_context,
            )
            .await;

        //
        // Ensure that the subscription update isn't out of order, i.e.
        // we already hold a newer version of the account in our bank.
        //
        // The stricter intent is to ignore non-advancing subscription updates: if the bank
        // already has the account at the same slot, then a normal/plain update at that slot is
        // treated as stale/duplicate and should not overwrite local state, with the following
        // exception:
        //
        //  - In the undelegate/redelegate same-slot path, the bank can still hold a plain
        //    or undelegating version while the subscription update carries the delegated state
        //    at the same slot, so we must allow that update.
        //
        let reader = |in_bank: &AccountSharedData| {
            let bank_slot = in_bank.slot();
            let update_slot = account.read().slot();
            let same_slot_delegated_refresh = bank_slot == update_slot
                && account.read().is(AccountMode::Delegated)
                && (!in_bank.is(AccountMode::Delegated)
                    || in_bank.is(AccountMode::Transient));
            if bank_slot > update_slot
                || (bank_slot == update_slot && !same_slot_delegated_refresh)
            {
                Some(bank_slot)
            } else {
                None
            }
        };
        let non_advancing_slot = self.read_account(&pubkey, reader).flatten();

        if let Some(in_bank_slot) = non_advancing_slot {
            let update_slot = account.read().slot();
            if in_bank_slot == update_slot
                && let Some(projected_ata_clone_request) =
                    projected_ata_clone_request
                && let Err(err) = self
                    .clone_projected_ata_request(
                        projected_ata_clone_request,
                        subscription_clone_context,
                        AccountMaterialization::Update,
                    )
                    .await
            {
                warn!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to clone projected ATA from out-of-order delegated eATA update"
                );
            }
            trace!(
                pubkey = %pubkey,
                bank_slot = in_bank_slot,
                update_slot,
                "Ignoring out-of-order subscription update"
            );
            return;
        }

        let mut undelegation_completed_on_chain = false;
        let reader = |in_bank: &AccountSharedData| {
            (
                in_bank.is(AccountMode::Delegated),
                in_bank.is(AccountMode::Transient),
                *in_bank.owner(),
                in_bank.slot(),
            )
        };
        if let Some((delegated, transient, owner, slot)) =
            self.read_account(&pubkey, reader)
        {
            if delegated && !transient {
                self.cleanup_direct_subscription_for_delegated_account(pubkey)
                    .await;
                return;
            }

            if transient {
                debug!(
                    pubkey = %pubkey,
                    in_bank_delegated = delegated,
                    in_bank_owner = %owner,
                    in_bank_slot = slot,
                    chain_delegated = account.read().is(AccountMode::Delegated),
                    chain_owner = %account.read().owner(),
                    chain_slot = account.read().slot(),
                    "Received update for undelegating account"
                );

                if account.read().is(AccountMode::Delegated)
                    && ata_projection::derive_eata_pubkey_from_ata_account(
                        &pubkey, &account,
                    )
                    .is_some()
                    && deleg_record.as_ref().is_some_and(|record| {
                        record.owner == EATA_PROGRAM_ID
                            && record.authority == self.validator_pubkey
                    })
                {
                    debug!(
                        pubkey = %pubkey,
                        "Keeping undelegating ATA in bank while companion eATA remains delegated"
                    );
                    return;
                }

                // This will only be true in the following case:
                // 1. a commit was triggered for the account
                // 2. a commit + undelegate was triggered for the account -> undelegating
                // 3. we receive the update for (1.)
                //
                // Thus our state is more up to date and we don't
                // need to update our bank.
                if account_still_undelegating_on_chain(
                    &pubkey,
                    account.read().is(AccountMode::Delegated),
                    slot,
                    deleg_record,
                    &self.validator_pubkey,
                ) {
                    return;
                }
                undelegation_completed_on_chain = true;
            } else if !delegated && account.read().is(AccountMode::Delegated) {
                undelegation_completed_on_chain = true;
            } else if owner == dlp_api::id() {
                debug!(
                    pubkey = %pubkey,
                    "Received update for account owned by delegation program but not marked as undelegating"
                );
            }
        } else {
            debug!(
                pubkey = %pubkey,
                "Received update for account not in bank"
            );
            if account.read().is(AccountMode::Delegated) {
                undelegation_completed_on_chain = true;
            }
        }

        // Determine if delegated to another validator
        let delegated_to_other = deleg_record
            .as_ref()
            .and_then(|dr| self.get_delegated_to_other(dr));

        // Delegated subscription cleanup is limited to direct subscription/pubsub tracking
        // ownership here; undelegation tracking owns protected subscriptions
        // until undelegation is explicitly complete.
        if undelegation_completed_on_chain {
            if !account.read().is(AccountMode::Delegated) {
                self.ensure_direct_subscription_for_completed_account(pubkey)
                    .await;
            }
            self.cleanup_undelegation_tracking_for_completed_account(pubkey)
                .await;
        }
        if account.read().is(AccountMode::Delegated) {
            self.cleanup_direct_subscription_for_delegated_account(pubkey)
                .await;
        }

        if account.read().flags().contains(StateFlags::EXECUTABLE) {
            self.handle_executable_sub_update(
                pubkey,
                account,
                &companion_fetch_log_context,
            )
            .await;
        } else {
            let commit_frequency_ms = deleg_record.as_ref().and_then(|dr| {
                dr.authority
                    .eq(&self.validator_pubkey)
                    .then_some(dr.commit_frequency_ms)
            });
            if let Err(err) = self
                .clone_account_with_post_delegation_action_invariants(
                    AccountCloneRequest {
                        pubkey,
                        account,
                        commit_frequency_ms,
                        post_delegation_mode: ClonePostDelegationMode::None,
                        delegated_to_other,
                    },
                    subscription_clone_context.clone(),
                    AccountMaterialization::Update,
                )
                .await
            {
                error!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to clone account into bank"
                );
            } else if let Some(projected_ata_clone_request) =
                projected_ata_clone_request
                && let Err(err) = self
                    .clone_projected_ata_request(
                        projected_ata_clone_request,
                        subscription_clone_context,
                        AccountMaterialization::Update,
                    )
                    .await
            {
                error!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to clone projected ATA from delegated eATA update"
                );
            }
        }
    }
}

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    pub(super) fn ensure_delegation_action_dependencies<'a>(
        &'a self,
        pubkey: Pubkey,
        remote_slot: u64,
        delegation_actions: &'a DelegationActions,
        fetch_context: AccountFetchContext,
    ) -> Pin<Box<dyn Future<Output = ChainlinkResult<()>> + Send + 'a>> {
        Box::pin(async move {
            if delegation_actions.is_empty() {
                return Ok(());
            }

            self.validate_post_delegation_action_signers(delegation_actions)
                .await?;

            let (dependencies, writable_dependencies) =
                Self::collect_post_delegation_action_dependencies(
                    pubkey,
                    delegation_actions,
                );

            let dependencies_to_fetch = {
                let accessor = self.engine.accounts();
                let loader = accessor.loader();
                dependencies
                    .into_iter()
                    .filter(|dependency| {
                        let reader = |account: &AccountSharedData| {
                            // A copy that already sits at `remote_slot` is as
                            // fresh as the update we are resolving. Refreshing
                            // it anyway re-clones it at its current slot, and
                            // the engine rejects that slot patch as a
                            // non-advancing transition, which fails the whole
                            // action path and undelegates the account.
                            account.slot() < remote_slot
                                && writable_dependencies.contains(dependency)
                                && (!account.is(AccountMode::Delegated)
                                    || account.is(AccountMode::Transient))
                        };
                        let Some(needs_refresh) =
                            loader.read(dependency, reader).ok().flatten()
                        else {
                            return true;
                        };
                        needs_refresh
                    })
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            };

            if dependencies_to_fetch.is_empty() {
                return Ok(());
            }

            let result = self
                .fetch_and_clone_accounts_with_dedup_forced_refresh(
                    &dependencies_to_fetch,
                    None,
                    Some(remote_slot),
                    fetch_context.with_reason(
                        AccountFetchReason::ActionDependencyForcedRefresh,
                    ),
                    &writable_dependencies,
                    None,
                )
                .await?;
            if result.missing_delegation_record.is_empty() {
                return Ok(());
            }

            let missing_accounts = result
                .pubkeys_missing_delegation_record()
                .into_iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let mut missing_accounts = missing_accounts;
            missing_accounts.sort_unstable();
            Err(ChainlinkError::MissingDelegationActionAccounts(
                missing_accounts,
            ))
        })
    }

    pub(super) fn collect_post_delegation_action_dependencies(
        target: Pubkey,
        delegation_actions: &DelegationActions,
    ) -> (HashSet<Pubkey>, HashSet<Pubkey>) {
        let mut dependencies = HashSet::new();
        let mut writable_dependencies = HashSet::new();
        for instruction in delegation_actions.iter() {
            if instruction.program_id != target {
                dependencies.insert(instruction.program_id);
            }
            for meta in &instruction.accounts {
                if meta.pubkey == target {
                    continue;
                }
                dependencies.insert(meta.pubkey);
                if meta.is_writable {
                    writable_dependencies.insert(meta.pubkey);
                }
            }
        }
        (dependencies, writable_dependencies)
    }

    pub(super) async fn validate_post_delegation_action_signers(
        &self,
        delegation_actions: &DelegationActions,
    ) -> ChainlinkResult<()> {
        let Some(risk_service) = self.risk_service.as_ref() else {
            return Ok(());
        };

        let mut signers = delegation_actions
            .iter()
            .flat_map(|instruction| {
                instruction.accounts.iter().filter_map(|meta| {
                    if meta.is_signer {
                        Some(meta.pubkey.to_string())
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();
        signers.sort_unstable();
        signers.dedup();

        if signers.is_empty() {
            return Ok(());
        }
        Ok(risk_service.check_addresses(signers).await?)
    }

    pub(super) async fn clone_projected_ata_request(
        &self,
        request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
        materialization: AccountMaterialization,
    ) -> ChainlinkResult<()> {
        match self.read_account(&request.pubkey, |account| {
            account.is(AccountMode::Transient)
        }) {
            Some(true) => {
                return Ok(());
            }
            None if materialization == AccountMaterialization::Update => {
                trace!(
                    pubkey = %request.pubkey,
                    "Ignoring projected ATA update for account absent from bank"
                );
                return Ok(());
            }
            _ => {}
        }

        self.clone_account_with_post_delegation_action_invariants(
            request,
            fetch_context.with_reason(AccountFetchReason::AtaProjection),
            materialization,
        )
        .await
        .map(drop)
    }

    pub(super) async fn resolve_internal_dlp_collision(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) {
        let Some(mirror) = &self.record_mirror else {
            trace!(
                pubkey = %pubkey,
                "Dropping internal DLP program update without a record mirror"
            );
            return;
        };
        let record_pda = delegation_record_pda_from_delegated_account(&pubkey);
        match mirror.probe(&record_pda, slot) {
            MirrorLookup::Hit { .. } => {
                if self.clone_colliding_delegated_account(pubkey, slot).await
                    == CollisionCloneOutcome::Retry
                {
                    self.schedule_collision_recheck(pubkey, slot).await;
                }
            }
            MirrorLookup::Tombstone { .. } => {}
            MirrorLookup::Miss => {
                self.schedule_collision_recheck(pubkey, slot).await;
            }
        }
    }

    pub(super) fn schedule_collision_recheck(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.schedule_collision_recheck_inner(pubkey, slot))
    }

    async fn schedule_collision_recheck_inner(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) {
        let record_pda = delegation_record_pda_from_delegated_account(&pubkey);
        let enqueue = {
            let mut rechecks = self.pending_collision_rechecks.lock();
            try_enqueue_collision_recheck(
                &mut rechecks,
                record_pda,
                pubkey,
                slot,
                PENDING_COLLISION_RECHECKS_CAPACITY.get(),
            )
        };
        match enqueue {
            CollisionRecheckEnqueue::Inserted => {}
            CollisionRecheckEnqueue::Updated => return,
            CollisionRecheckEnqueue::Full => {
                trace!(
                    pubkey = %pubkey,
                    slot,
                    "Collision recheck queue is full; reconciling via RPC"
                );
                // Overflow work does not wait for mirror progress, so this
                // backpressure cannot form a cycle with the mirror consumer.
                // This semaphore is owned here and never closed, so acquiring
                // a permit cannot fail.
                let permit = self
                    .collision_overflow_reconciliations
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("collision overflow semaphore never closed");
                let this = self.clone();
                task::spawn(async move {
                    let _permit = permit;
                    let mut retry_count = 0usize;
                    loop {
                        let Some(mirror) = &this.record_mirror else {
                            return;
                        };
                        if collision_overflow_action(
                            &mirror.probe(&record_pda, slot),
                            mirror.is_complete_through(slot),
                        ) == CollisionRecheckAction::Discard
                        {
                            return;
                        }
                        match this
                            .clone_colliding_delegated_account(pubkey, slot)
                            .await
                        {
                            CollisionCloneOutcome::Settled
                            | CollisionCloneOutcome::Terminal => return,
                            CollisionCloneOutcome::Retry => {
                                let delay = COLLISION_RECHECK_DELAYS
                                    [retry_count.min(
                                        COLLISION_RECHECK_DELAYS.len() - 1,
                                    )];
                                retry_count = retry_count.saturating_add(1);
                                tokio::time::sleep(delay).await;
                            }
                        }
                    }
                });
                return;
            }
        }

        let this = self.clone();
        task::spawn(async move {
            let mut probe_count = 0usize;
            loop {
                let delay = COLLISION_RECHECK_DELAYS
                    [probe_count.min(COLLISION_RECHECK_DELAYS.len() - 1)];
                tokio::time::sleep(delay).await;

                let candidate = this
                    .pending_collision_rechecks
                    .lock()
                    .peek(&record_pda)
                    .copied();
                let Some((pubkey, slot)) = candidate else {
                    return;
                };
                let Some(mirror) = &this.record_mirror else {
                    return;
                };
                let action = collision_recheck_action(
                    &mirror.probe(&record_pda, slot),
                    mirror.is_complete_through(slot),
                );
                if action == CollisionRecheckAction::Wait {
                    probe_count = probe_count.saturating_add(1);
                    continue;
                }

                let owned = {
                    let mut rechecks = this.pending_collision_rechecks.lock();
                    let matches = rechecks
                        .peek(&record_pda)
                        .is_some_and(|&(_, pending_slot)| pending_slot == slot);
                    if matches {
                        rechecks.pop(&record_pda);
                    }
                    matches
                };
                if !owned {
                    probe_count = 0;
                    continue;
                }
                if action == CollisionRecheckAction::Resolve
                    && this
                        .clone_colliding_delegated_account(pubkey, slot)
                        .await
                        == CollisionCloneOutcome::Retry
                {
                    this.schedule_collision_recheck(pubkey, slot).await;
                }
                return;
            }
        });
    }

    async fn clone_colliding_delegated_account(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) -> CollisionCloneOutcome {
        let fresh_delegated = self
            .read_account(&pubkey, |account| {
                account.is(AccountMode::Delegated) && account.slot() >= slot
            })
            .unwrap_or(false);
        if fresh_delegated {
            return CollisionCloneOutcome::Settled;
        }

        let fetch_context = AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
        );
        let record_context = fetch_context
            .clone()
            .with_reason(AccountFetchReason::DelegationRecord);
        let record_min_context_slot =
            slot.max(self.remote_account_provider.chain_slot());
        let deleg_record = match delegation::resolve_delegation_record(
            self,
            pubkey,
            record_min_context_slot,
            record_context.clone(),
            &CompanionFetchLogContext {
                origin: record_context,
                primary_pubkey: pubkey,
                context_slot: record_min_context_slot,
            },
        )
        .await
        {
            delegation::DelegationRecordResolution::Found(record, _) => record,
            delegation::DelegationRecordResolution::Missing => {
                return CollisionCloneOutcome::Terminal;
            }
            delegation::DelegationRecordResolution::Invalid => {
                return CollisionCloneOutcome::Terminal;
            }
            delegation::DelegationRecordResolution::Uncertain => {
                return CollisionCloneOutcome::Retry;
            }
        };
        if deleg_record.authority != self.validator_pubkey
            && deleg_record.authority != Pubkey::default()
        {
            metrics::inc_discovered_dlp_update_delegated_elsewhere();
            return CollisionCloneOutcome::Settled;
        }

        const MAX_CLONE_ATTEMPTS: usize = 3;
        for _ in 0..MAX_CLONE_ATTEMPTS {
            let still_undelegating = self
                .read_account(&pubkey, |account| {
                    account.is(AccountMode::Transient)
                        && account_still_undelegating_on_chain(
                            &pubkey,
                            true,
                            account.slot(),
                            Some(deleg_record),
                            &self.validator_pubkey,
                        )
                })
                .unwrap_or(false);
            if still_undelegating {
                return CollisionCloneOutcome::Settled;
            }

            let result = match self
                .fetch_and_clone_accounts_with_dedup_forced_refresh(
                    &[pubkey],
                    None,
                    Some(slot),
                    fetch_context.clone(),
                    &HashSet::from([pubkey]),
                    Some((pubkey, deleg_record.delegation_slot)),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    warn!(pubkey = %pubkey, %error, "Collision clone failed");
                    return CollisionCloneOutcome::Retry;
                }
            };
            if result
                .not_found_on_chain
                .iter()
                .chain(result.missing_delegation_record.iter())
                .any(|(missing, _)| missing == &pubkey)
            {
                return CollisionCloneOutcome::Terminal;
            }
            let state = self.read_account(&pubkey, |account| {
                let settled = account.slot() > slot
                    || (account.slot() == slot
                        && account.is(AccountMode::Delegated));
                let stale_transient = account.is(AccountMode::Transient)
                    && deleg_record.delegation_slot <= account.slot();
                (settled, stale_transient)
            });
            if state.is_some_and(|(settled, _)| settled) {
                return CollisionCloneOutcome::Settled;
            }
            if state.is_some_and(|(_, stale)| stale) {
                break;
            }
        }

        warn!(
            pubkey = %pubkey,
            slot,
            "Collision clone did not settle; waiting for newer record evidence"
        );
        CollisionCloneOutcome::Retry
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollisionRecheckEnqueue {
    Inserted,
    Updated,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollisionRecheckAction {
    Wait,
    Resolve,
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollisionCloneOutcome {
    Settled,
    Terminal,
    Retry,
}

fn collision_recheck_action(
    lookup: &MirrorLookup,
    complete_through_slot: bool,
) -> CollisionRecheckAction {
    match lookup {
        MirrorLookup::Hit { .. } => CollisionRecheckAction::Resolve,
        MirrorLookup::Tombstone { .. } => CollisionRecheckAction::Discard,
        MirrorLookup::Miss if complete_through_slot => {
            CollisionRecheckAction::Resolve
        }
        MirrorLookup::Miss => CollisionRecheckAction::Wait,
    }
}

fn collision_overflow_action(
    lookup: &MirrorLookup,
    complete_through_slot: bool,
) -> CollisionRecheckAction {
    match lookup {
        MirrorLookup::Tombstone { .. } => CollisionRecheckAction::Discard,
        MirrorLookup::Miss if complete_through_slot => {
            CollisionRecheckAction::Discard
        }
        MirrorLookup::Hit { .. } | MirrorLookup::Miss => {
            CollisionRecheckAction::Resolve
        }
    }
}

fn try_enqueue_collision_recheck(
    rechecks: &mut LruCache<Pubkey, (Pubkey, u64)>,
    record_pda: Pubkey,
    pubkey: Pubkey,
    slot: u64,
    capacity: usize,
) -> CollisionRecheckEnqueue {
    if let Some(pending) = rechecks.get_mut(&record_pda) {
        pending.1 = pending.1.max(slot);
        return CollisionRecheckEnqueue::Updated;
    }
    if rechecks.len() >= capacity {
        return CollisionRecheckEnqueue::Full;
    }
    rechecks.put(record_pda, (pubkey, slot));
    CollisionRecheckEnqueue::Inserted
}

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    pub(super) async fn maybe_greedily_clone_discovered_delegated_account(
        &self,
        pubkey: Pubkey,
        update: &ForwardedSubscriptionUpdate,
    ) -> bool {
        if self.contains_account(&pubkey) {
            return false;
        }

        let Some(account) = update.account.fresh_account() else {
            return false;
        };

        if account.owner() != &dlp_api::id() {
            return false;
        }

        let discovery_context = AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
        );
        let record_context = discovery_context
            .clone()
            .with_reason(AccountFetchReason::DelegationRecord);

        let Some((deleg_record, delegation_actions)) = self
            .fetch_and_parse_delegation_record(
                pubkey,
                account.slot(),
                record_context.clone(),
                CompanionFetchLogContext {
                    origin: record_context,
                    primary_pubkey: pubkey,
                    context_slot: account.slot(),
                },
            )
            .await
        else {
            trace!(
                pubkey = %pubkey,
                slot = account.slot(),
                "Greedy discovery could not resolve delegation record; falling back"
            );
            return false;
        };

        let is_delegated_to_us = deleg_record.authority
            == self.validator_pubkey
            || deleg_record.authority == Pubkey::default();
        if !is_delegated_to_us {
            metrics::inc_discovered_dlp_update_delegated_elsewhere();
            trace!(
                pubkey = %pubkey,
                authority = %deleg_record.authority,
                "Ignoring discovered DLP-owned update delegated elsewhere"
            );
            return true;
        }
        let delegation_actions = delegation_actions.unwrap_or_default();

        let greedy_ata_pubkeys = delegation::parse_raw_eata_pda(
            &pubkey,
            account.data(),
            deleg_record.owner,
        )
        .map(|(wallet_owner, mint)| {
            ata_projection::derive_supported_ata_pubkeys(&wallet_owner, &mint)
        })
        .unwrap_or_default();
        let mut pubkeys_to_clone =
            Vec::with_capacity(1 + greedy_ata_pubkeys.len());
        pubkeys_to_clone.push(pubkey);
        {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            pubkeys_to_clone.extend(greedy_ata_pubkeys.iter().copied().filter(
                |ata_pubkey| !loader.contains(ata_pubkey).unwrap_or(false),
            ));
        }

        // Keep eATA discovery with its candidate base ATAs in one clone batch
        // so the normal ATA projection path runs for the same update.
        let clone_result = if greedy_ata_pubkeys.is_empty() {
            self.fetch_and_clone_accounts_with_dedup_forced_refresh(
                &pubkeys_to_clone,
                None,
                Some(account.slot()),
                discovery_context.clone(),
                &HashSet::new(),
                None,
            )
            .await
        } else {
            self.fetch_and_clone_accounts(
                &pubkeys_to_clone,
                None,
                Some(account.slot()),
                discovery_context.clone(),
            )
            .await
            .map(|batch| batch.result)
        };

        match clone_result {
            Ok(result)
                if result
                    .not_found_on_chain
                    .iter()
                    .all(|(missing_pubkey, _)| missing_pubkey != &pubkey)
                    && result.missing_delegation_record.iter().all(
                        |(missing_pubkey, _)| missing_pubkey != &pubkey,
                    ) =>
            {
                let bank_slot =
                    self.read_account(&pubkey, |in_bank| in_bank.slot());
                if bank_slot.is_none_or(|slot| slot < account.slot()) {
                    trace!(
                        pubkey = %pubkey,
                        bank_slot,
                        update_slot = account.slot(),
                        ?result,
                        "Greedy clone did not materialize a fresh enough account; falling back"
                    );
                    false
                } else if let Some(projected_ata_clone_request) = self
                    .maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
                        pubkey,
                        &AccountBuilder::from(AccountSharedData::from(
                            account.owned(),
                        )),
                        update.source,
                        Some(&deleg_record),
                        &delegation_actions,
                        &CompanionFetchLogContext {
                            origin: discovery_context.clone(),
                            primary_pubkey: pubkey,
                            context_slot: account.slot(),
                        },
                    )
                    .await
                {
                    let projected_ata_pubkey =
                        projected_ata_clone_request.pubkey;
                    if let Err(err) = self
                        .clone_projected_ata_request(
                            projected_ata_clone_request,
                            discovery_context.clone(),
                            AccountMaterialization::Create,
                        )
                        .await
                    {
                        warn!(
                            pubkey = %pubkey,
                            error = %err,
                            "Failed to clone projected ATA from greedily discovered delegated eATA"
                        );
                        false
                    } else {
                        trace!(
                            pubkey = %pubkey,
                            ata_pubkey = %projected_ata_pubkey,
                            slot = account.slot(),
                            "Greedily cloned delegated account"
                        );
                        true
                    }
                } else {
                    let cloned_ata_pubkey = {
                        let accessor = self.engine.accounts();
                        let loader = accessor.loader();
                        greedy_ata_pubkeys.iter().copied().find(|ata_pubkey| {
                            loader
                                .read(ata_pubkey, |account_in_bank| {
                                    account_in_bank.slot()
                                        >= account.slot()
                                })
                                .ok()
                                .flatten()
                                .unwrap_or(false)
                        })
                    };
                    if let Some(ata_pubkey) = cloned_ata_pubkey {
                        trace!(
                            pubkey = %pubkey,
                            ata_pubkey = %ata_pubkey,
                            slot = account.slot(),
                            "Greedily cloned delegated account"
                        );
                    } else {
                        trace!(
                            pubkey = %pubkey,
                            slot = account.slot(),
                            "Greedily cloned delegated account"
                        );
                    }
                    true
                }
            }
            Ok(result) => {
                trace!(
                    pubkey = %pubkey,
                    ?result,
                    "Greedy clone incomplete; falling back"
                );
                false
            }
            Err(err) => {
                warn!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to greedily clone discovered delegated account"
                );
                false
            }
        }
    }

    pub(super) async fn handle_executable_sub_update(
        &self,
        pubkey: Pubkey,
        account: AccountBuilder,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) {
        // moved to program_loader module
        program_loader::handle_executable_sub_update_with_context(
            self,
            pubkey,
            account,
            companion_fetch_log_context,
        )
        .await;
    }

    pub(super) async fn cleanup_direct_subscription_for_delegated_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .release_subscription_reason_silently_for_delegated_account(
                &pubkey,
                SubscriptionReason::DirectAccount,
            )
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to clean up direct subscription for delegated account"
            );
        }
    }

    pub(super) async fn ensure_direct_subscription_for_completed_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .ensure_subscription(&pubkey, SubscriptionReason::DirectAccount)
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to retain direct subscription for completed account"
            );
        }
    }

    pub(super) async fn cleanup_undelegation_tracking_for_completed_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .release_subscription_reason_silently_for_delegated_account(
                &pubkey,
                SubscriptionReason::UndelegationTracking,
            )
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to clean up undelegation tracking for completed account"
            );
        }
    }
}

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    pub(super) async fn resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
        &self,
        update: ForwardedSubscriptionUpdate,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> (
        Option<AccountBuilder>,
        Option<DelegationRecord>,
        DelegationActions,
    ) {
        let ForwardedSubscriptionUpdate {
            pubkey,
            account,
            source: _,
        } = update;
        let owned_by_delegation_program =
            account.is_owned_by_delegation_program();

        if let Some(account) = account.into_fresh_account() {
            // If the account is owned by the delegation program we need to resolve
            // its true owner and determine if it is delegated to us
            if owned_by_delegation_program {
                let delegation_record_pubkey =
                    delegation_record_pda_from_delegated_account(&pubkey);

                let acquired_delegation_record_reason = self
                    .acquire_subscription_reason(
                        &delegation_record_pubkey,
                        SubscriptionReason::DelegationRecord,
                    )
                    .await
                    .map(|_| true)
                    .unwrap_or_else(|err| {
                        warn!(
                            pubkey = %delegation_record_pubkey,
                            error = ?err,
                            "Failed to acquire delegation record subscription reason"
                        );
                        false
                    });

                match self
                    .task_to_fetch_with_companion(
                        pubkey,
                        delegation_record_pubkey,
                        account.slot(),
                        AccountFetchContext::subscription_update(
                            AccountFetchReason::DelegationRecord,
                        ),
                        ChainlinkCompanionFetchKind::DelegationRecord,
                        false,
                    )
                    .await
                {
                    Ok(Ok(AccountWithCompanion {
                        pubkey,
                        account,
                        companion_pubkey: delegation_record_pubkey,
                        companion_account: delegation_record,
                    })) => {
                        // We may need to remove temporary subscriptions created
                        // while resolving this update.
                        let mut subs_to_remove = Vec::new();

                        subs_to_remove.push(SubscriptionRelease::Pubkey {
                            pubkey: delegation_record_pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        });
                        if acquired_delegation_record_reason {
                            subs_to_remove.push(SubscriptionRelease::Pubkey {
                                pubkey: delegation_record_pubkey,
                                reason: SubscriptionReason::DelegationRecord,
                            });
                        }

                        let account = if let Some(delegation_record) =
                            delegation_record
                        {
                            let delegation_record_with_actions = match self
                                .parse_delegation_record(
                                    delegation_record.read().data(),
                                    delegation_record_pubkey,
                                ) {
                                Ok(x) => Some(x),
                                Err(err) => {
                                    error!(
                                        pubkey = %pubkey,
                                        error = %err,
                                        "Failed to parse delegation record"
                                    );
                                    None
                                }
                            };

                            // If the delegation record is valid we set the owner and delegation
                            // status on the account
                            if let Some((
                                delegation_record,
                                delegation_actions,
                            )) = delegation_record_with_actions
                            {
                                if tracing::enabled!(tracing::Level::TRACE) {
                                    let delegation_record_display =
                                        format!("{:?}", delegation_record);
                                    trace!(
                                        pubkey = %pubkey,
                                        slot = account.read().slot(),
                                        owner = %delegation_record.owner,
                                        deleg_record = %delegation_record_display,
                                        "Resolving delegated account"
                                    );
                                }

                                let account = self
                                    .apply_delegation_record_to_account(
                                        pubkey,
                                        account,
                                        &delegation_record,
                                    )
                                    .0;

                                // For accounts delegated to us, subscribe to the original owner
                                // program for undelegation update resilience.
                                if account
                                    .read()
                                    .is(AccountMode::Delegated)
                                    && !self.program_subscription_is_too_broad(
                                        &delegation_record.owner,
                                    )
                                {
                                    // Fire-and-forget to avoid blocking subscription updates.
                                    let provider =
                                        self.remote_account_provider.clone();
                                    let owner = delegation_record.owner;
                                    tokio::spawn(async move {
                                        if let Err(err) = provider
                                            .subscribe_program(owner)
                                            .await
                                        {
                                            warn!(
                                                "Failed to subscribe to owner program {} for account {}: {}",
                                                owner, pubkey, err
                                            );
                                        }
                                    });
                                }

                                (
                                    Some(account),
                                    Some(delegation_record),
                                    delegation_actions.unwrap_or_default(),
                                )
                            } else {
                                // If the delegation record is invalid we cannot clone the account
                                // since something is corrupt and we wouldn't know what owner to
                                // use, etc.
                                (None, None, DelegationActions::default())
                            }
                        } else if let Ok(request) =
                            UndelegationRequest::try_from_bytes_with_discriminator(
                                account.read().data(),
                            )
                        {
                            let observed = ObservedUndelegationRequest {
                                request_pda: pubkey,
                                delegated_account: request.delegated_account,
                                expires_at_slot: request.expires_at_slot,
                                observed_slot: account.read().slot(),
                            };
                            trace!(
                                request_pda = %observed.request_pda,
                                delegated_account = %observed.delegated_account,
                                expires_at_slot = observed.expires_at_slot,
                                "Observed DLP undelegation request"
                            );
                            if let Err(mpsc::error::SendError(observed)) = self
                                .undelegation_request_sender
                                .send(observed)
                                .await
                            {
                                warn!(
                                    request_pda = %observed.request_pda,
                                    delegated_account = %observed.delegated_account,
                                    observed_slot = observed.observed_slot,
                                    expires_at_slot = observed.expires_at_slot,
                                    drop_reason = "request_consumer_closed",
                                    "Dropped observed DLP undelegation request because its consumer is closed"
                                );
                            }
                            (
                                Some(account),
                                None,
                                DelegationActions::default(),
                            )
                        } else if is_internal_dlp_account_data(
                            account.read().data(),
                        ) {
                            (
                                Some(account),
                                None,
                                DelegationActions::default(),
                            )
                        } else {
                            trace!(
                                pubkey = %pubkey,
                                "Skipping DLP-owned subscription update without delegation record"
                            );
                            (None, None, DelegationActions::default())
                        };

                        if !subs_to_remove.is_empty() {
                            release_subs(
                                &self.remote_account_provider,
                                subs_to_remove,
                            )
                            .await;
                        }
                        account
                    }
                    // In case of errors fetching the delegation record we cannot clone the account
                    Ok(Err(err)) => {
                        log_companion_fetch_failure(
                            companion_fetch_log_context,
                            delegation_record_pubkey,
                            ChainlinkCompanionFetchKind::DelegationRecord,
                            &err,
                        );
                        if acquired_delegation_record_reason {
                            release_subs(
                                &self.remote_account_provider,
                                [SubscriptionRelease::Pubkey {
                                    pubkey: delegation_record_pubkey,
                                    reason:
                                        SubscriptionReason::DelegationRecord,
                                }],
                            )
                            .await;
                        }
                        (None, None, DelegationActions::default())
                    }
                    Err(err) => {
                        log_companion_fetch_failure(
                            companion_fetch_log_context,
                            delegation_record_pubkey,
                            ChainlinkCompanionFetchKind::DelegationRecord,
                            &err,
                        );
                        if acquired_delegation_record_reason {
                            release_subs(
                                &self.remote_account_provider,
                                [SubscriptionRelease::Pubkey {
                                    pubkey: delegation_record_pubkey,
                                    reason:
                                        SubscriptionReason::DelegationRecord,
                                }],
                            )
                            .await;
                        }
                        (None, None, DelegationActions::default())
                    }
                }
            } else {
                let (account, deleg_record) = self
                    .maybe_project_ata_from_subscription_update(
                        pubkey,
                        AccountBuilder::from(account),
                        companion_fetch_log_context,
                    )
                    .await;
                if let Some((deleg_record, actions)) = deleg_record {
                    (
                        Some(account),
                        Some(deleg_record),
                        actions.unwrap_or_default(),
                    )
                } else {
                    (Some(account), None, DelegationActions::default())
                }
            }
        } else {
            // This should not happen since we call this method with sub updates which always hold
            // a fresh remote account
            error!(pubkey = %pubkey, "BUG: Received subscription update without fresh account");
            (None, None, DelegationActions::default())
        }
    }

    pub(super) async fn maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
        &self,
        eata_pubkey: Pubkey,
        eata_account: &AccountBuilder,
        update_source: SubscriptionSource,
        deleg_record: Option<&DelegationRecord>,
        delegation_actions: &DelegationActions,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> Option<AccountCloneRequest> {
        ata_projection::maybe_build_projected_ata_clone_request_from_subscription_update(
            self,
            eata_pubkey,
            eata_account,
            update_source,
            deleg_record,
            delegation_actions,
            companion_fetch_log_context,
        )
        .await
    }

    pub(super) async fn maybe_project_ata_from_subscription_update(
        &self,
        ata_pubkey: Pubkey,
        ata_account: AccountBuilder,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> (
        AccountBuilder,
        Option<(DelegationRecord, Option<DelegationActions>)>,
    ) {
        ata_projection::maybe_project_ata_from_subscription_update(
            self,
            ata_pubkey,
            ata_account,
            companion_fetch_log_context,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_recheck_waits_for_slot_completeness() {
        assert_eq!(
            collision_recheck_action(&MirrorLookup::Miss, false),
            CollisionRecheckAction::Wait
        );
        assert_eq!(
            collision_recheck_action(&MirrorLookup::Miss, true),
            CollisionRecheckAction::Resolve
        );
        assert_eq!(
            collision_recheck_action(
                &MirrorLookup::Tombstone { slot: 1 },
                false
            ),
            CollisionRecheckAction::Discard
        );
        assert_eq!(
            collision_overflow_action(&MirrorLookup::Miss, true),
            CollisionRecheckAction::Discard
        );
        assert_eq!(
            collision_overflow_action(&MirrorLookup::Miss, false),
            CollisionRecheckAction::Resolve
        );
        assert_eq!(
            collision_overflow_action(
                &MirrorLookup::Tombstone { slot: 1 },
                false
            ),
            CollisionRecheckAction::Discard
        );
    }

    #[test]
    fn collision_recheck_capacity_never_evicts_a_candidate() {
        let mut rechecks = LruCache::new(NonZeroUsize::new(1).unwrap());
        let first = Pubkey::new_unique();
        let first_record = delegation_record_pda_from_delegated_account(&first);
        assert_eq!(
            try_enqueue_collision_recheck(
                &mut rechecks,
                first_record,
                first,
                1,
                1,
            ),
            CollisionRecheckEnqueue::Inserted
        );

        let second = Pubkey::new_unique();
        assert_eq!(
            try_enqueue_collision_recheck(
                &mut rechecks,
                delegation_record_pda_from_delegated_account(&second),
                second,
                2,
                1,
            ),
            CollisionRecheckEnqueue::Full
        );
        assert_eq!(rechecks.peek(&first_record), Some(&(first, 1)));

        assert_eq!(
            try_enqueue_collision_recheck(
                &mut rechecks,
                first_record,
                first,
                3,
                1,
            ),
            CollisionRecheckEnqueue::Updated
        );
        assert_eq!(rechecks.peek(&first_record), Some(&(first, 3)));
    }
}
