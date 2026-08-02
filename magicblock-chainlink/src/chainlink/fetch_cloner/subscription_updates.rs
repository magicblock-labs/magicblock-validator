//! Subscription-update processing: forwarded subs, collisions, greedy cloning, record parsing.

use std::{collections::HashSet, sync::Arc};

use dlp_api::{
    pda::delegation_record_pda_from_delegated_account,
    state::{DelegationRecord, UndelegationRequest},
};
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::EATA_PROGRAM_ID;
use magicblock_metrics::metrics::{
    self, AccountFetchContext, AccountFetchReason, ChainlinkCompanionFetchKind,
};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tokio::{
    sync::{broadcast, mpsc, Semaphore},
    task,
    task::JoinSet,
};
use tracing::*;

use super::*;

impl<T, U, V, C> FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    pub fn start_subscription_listener(
        self: Arc<Self>,
        mut subscription_updates: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) {
        tokio::spawn(async move {
            let semaphore = Arc::new(Semaphore::new(
                crate::chainlink::SUBSCRIPTION_UPDATE_LIMIT,
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
            .as_ref()
            .is_some_and(|account| account.owner() == &dlp_api::id());
        let is_internal_dlp_update =
            fresh_update_account.as_ref().is_some_and(|account| {
                is_internal_dlp_account_data(account.data())
            });

        let dlp_program_interest =
            if matches!(update.source, SubscriptionSource::Program)
                && is_dlp_owned_update
            {
                match fresh_update_account.as_ref() {
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

        // Internal DLP payloads (records/metadata/commit state) can never be
        // greedily cloned, so drop them before discovery issues remote
        // fetches. The exception is an account whose app data collides with
        // an internal discriminator: its delegation also writes the
        // delegation record, which the mirror observes on its own stream —
        // resolve via the mirror now, or after one short recheck covering
        // the skew between the account and record streams. Only the program
        // firehose is handled here; account-sourced internal payloads (e.g.
        // directly watched records) proceed to normal processing.
        if !matches!(
            dlp_program_interest,
            Some(DlpProgramUpdateInterest::ProcessUndelegating)
                | Some(DlpProgramUpdateInterest::ProcessAtaProjection)
        ) && is_dlp_owned_update
            && is_internal_dlp_update
            && matches!(update.source, SubscriptionSource::Program)
        {
            // Internal-shaped bytes cannot be told apart from a colliding
            // user account (record-shaped app data included), so every
            // payload is probed; genuine internal accounts have no record
            // at their derived PDA and drop after the recheck.
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
        // account-sub LRU is the source of truth for `is_watching`. Program
        // subscription updates can legitimately arrive for pubkeys that are
        // *not* in the account-sub LRU (e.g. delegated accounts whose direct
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
            if self.accounts_bank.get_account(&pubkey).is_some() {
                if let Err(err) = self
                    .remote_account_provider
                    .send_removal_update(pubkey)
                    .await
                {
                    warn!(
                        pubkey = %pubkey,
                        error = ?err,
                        "Failed to enqueue stale subscription update removal"
                    );
                }
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
        let non_advancing_slot =
            self.accounts_bank.get_account(&pubkey).and_then(|in_bank| {
                let bank_slot = in_bank.remote_slot();
                let update_slot = account.remote_slot();
                let same_slot_delegated_refresh = bank_slot == update_slot
                    && account.delegated()
                    && (!in_bank.delegated() || in_bank.undelegating());
                if bank_slot > update_slot
                    || (bank_slot == update_slot
                        && !same_slot_delegated_refresh)
                {
                    Some(bank_slot)
                } else {
                    None
                }
            });

        if let Some(in_bank_slot) = non_advancing_slot {
            let update_slot = account.remote_slot();
            if in_bank_slot == update_slot {
                if let Some(projected_ata_clone_request) =
                    projected_ata_clone_request
                {
                    if let Err(err) = self
                        .clone_projected_ata_request(
                            projected_ata_clone_request,
                            subscription_clone_context,
                        )
                        .await
                    {
                        warn!(
                            pubkey = %pubkey,
                            error = %err,
                            "Failed to clone projected ATA from out-of-order delegated eATA update"
                        );
                    }
                }
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
        if let Some(in_bank) = self.accounts_bank.get_account(&pubkey) {
            if in_bank.delegated() && !in_bank.undelegating() {
                self.cleanup_direct_subscription_for_delegated_account(pubkey)
                    .await;
                return;
            }

            if in_bank.undelegating() {
                debug!(
                    pubkey = %pubkey,
                    in_bank_delegated = in_bank.delegated(),
                    in_bank_owner = %in_bank.owner(),
                    in_bank_slot = in_bank.remote_slot(),
                    chain_delegated = account.delegated(),
                    chain_owner = %account.owner(),
                    chain_slot = account.remote_slot(),
                    "Received update for undelegating account"
                );

                if account.delegated()
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
                    account.delegated(),
                    in_bank.remote_slot(),
                    deleg_record,
                    &self.validator_pubkey,
                ) {
                    return;
                }
                undelegation_completed_on_chain = true;
            } else if !in_bank.delegated() && account.delegated() {
                undelegation_completed_on_chain = true;
            } else if in_bank.owner().eq(&dlp_api::id()) {
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
            if account.delegated() {
                undelegation_completed_on_chain = true;
            }
        }

        // Determine if delegated to another validator
        let delegated_to_other = deleg_record
            .as_ref()
            .and_then(|dr| self.get_delegated_to_other(dr));

        // Delegated subscription cleanup is limited to direct subscription/LRU
        // ownership here; undelegation tracking owns protected subscriptions
        // until undelegation is explicitly complete.
        if undelegation_completed_on_chain {
            if !account.delegated() {
                self.ensure_direct_subscription_for_completed_account(pubkey)
                    .await;
            }
            self.cleanup_undelegation_tracking_for_completed_account(pubkey)
                .await;
        }
        if account.delegated() {
            self.cleanup_direct_subscription_for_delegated_account(pubkey)
                .await;
        }

        if account.executable() {
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
            let raw_delegation_actions = if account.delegated()
                && projected_ata_clone_request.is_none()
            {
                delegation_actions
            } else {
                DelegationActions::default()
            };
            if let Err(err) = self
                .clone_account_with_post_delegation_action_invariants(
                    AccountCloneRequest {
                        pubkey,
                        account,
                        commit_frequency_ms,
                        delegation_actions: raw_delegation_actions,
                        delegated_to_other,
                        needs_undelegation: false,
                    },
                    subscription_clone_context.clone(),
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
            {
                if let Err(err) = self
                    .clone_projected_ata_request(
                        projected_ata_clone_request,
                        subscription_clone_context.clone(),
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

    pub(super) async fn ensure_delegation_action_dependencies(
        &self,
        pubkey: Pubkey,
        remote_slot: u64,
        delegation_actions: &DelegationActions,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<()> {
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

        let dependencies_to_fetch = dependencies
            .into_iter()
            .filter(|dependency| {
                self.delegation_action_dependency_needs_fetch(
                    dependency,
                    &writable_dependencies,
                )
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

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

    pub(super) fn delegation_action_dependency_needs_fetch(
        &self,
        dependency: &Pubkey,
        writable_dependencies: &HashSet<Pubkey>,
    ) -> bool {
        let Some(account) = self.accounts_bank.get_account(dependency) else {
            return true;
        };
        writable_dependencies.contains(dependency)
            && (!account.delegated() || account.undelegating())
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
    ) -> ChainlinkResult<Signature> {
        if self
            .accounts_bank
            .get_account(&request.pubkey)
            .is_some_and(|account| account.undelegating())
        {
            return Ok(Signature::default());
        }

        self.clone_account_with_post_delegation_action_invariants(
            request,
            fetch_context.with_reason(AccountFetchReason::AtaProjection),
        )
        .await
    }

    /// A program-source internal-looking update is either genuine internal
    /// DLP state or a delegated account whose app data collides with an
    /// internal discriminator. Its delegation writes the record in the same
    /// slot, so the mirror proves the collision: resolve immediately on a
    /// mirror hit, or once more after a short delay covering record-stream
    /// skew. A final miss drops the update — the on-demand ensure path
    /// remains the backstop, exactly as unsighted parked candidates were
    /// dropped before. Without a live mirror there is no proactive
    /// collision discovery.
    pub(super) async fn resolve_internal_dlp_collision(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) {
        let Some(mirror) = &self.record_mirror else {
            trace!(
                pubkey = %pubkey,
                "Dropping internal DLP program subscription update (no record mirror)"
            );
            return;
        };
        let record_pda = delegation_record_pda_from_delegated_account(&pubkey);
        if matches!(mirror.probe(&record_pda, slot), MirrorLookup::Hit { .. }) {
            self.clone_colliding_delegated_account(pubkey, slot).await;
        } else {
            self.schedule_collision_recheck(pubkey, slot);
        }
    }

    /// Parks a collision candidate for delayed mirror rechecks. Dedup is
    /// keyed by the derived record PDA with a monotonic slot. Admitted
    /// candidates are never displaced: at capacity, newcomers are dropped
    /// instead — parity with the parking this replaces, so genuine internal
    /// firehose churn cannot evict a real collision candidate. Exactly one
    /// worker owns each admitted entry, and only that worker removes it, so
    /// entries and worker tasks stay 1:1 bounded by the map capacity.
    pub(super) fn schedule_collision_recheck(&self, pubkey: Pubkey, slot: u64) {
        let record_pda = delegation_record_pda_from_delegated_account(&pubkey);
        {
            let mut rechecks = self.pending_collision_rechecks.lock();
            if let Some(pending) = rechecks.get_mut(&record_pda) {
                // A replayed older update must not downgrade the slot.
                pending.1 = pending.1.max(slot);
                return;
            }
            if rechecks.len() >= PENDING_COLLISION_RECHECKS_CAPACITY.get() {
                trace!(
                    pubkey = %pubkey,
                    slot,
                    "Dropping collision candidate; recheck queue is full"
                );
                return;
            }
            rechecks.put(record_pda, (pubkey, slot));
        }

        let this = self.clone();
        task::spawn(async move {
            // Probes for the current generation, i.e. consecutive attempts
            // that observed the same slot. A newer update bumps the slot in
            // place and restarts the budget; every bump is driven by a real
            // firehose update, which rate-limits the loop externally.
            let mut generation_probes = 0usize;
            let mut probed_slot = None;
            loop {
                let delay_idx =
                    generation_probes.min(COLLISION_RECHECK_DELAYS.len() - 1);
                tokio::time::sleep(COLLISION_RECHECK_DELAYS[delay_idx]).await;

                // Read fresh each attempt: later updates bump the slot.
                let candidate = this
                    .pending_collision_rechecks
                    .lock()
                    .peek(&record_pda)
                    .copied();
                let Some((pubkey, slot)) = candidate else {
                    return;
                };

                let hit = this.record_mirror.as_ref().is_some_and(|mirror| {
                    matches!(
                        mirror.probe(&record_pda, slot),
                        MirrorLookup::Hit { .. }
                    )
                });
                if hit {
                    // Remove only the generation that was probed: a newer
                    // update racing in keeps the entry for the next attempt.
                    let owned = {
                        let mut rechecks =
                            this.pending_collision_rechecks.lock();
                        let matches_probed = rechecks
                            .peek(&record_pda)
                            .is_some_and(|&(_, s)| s == slot);
                        if matches_probed {
                            rechecks.pop(&record_pda);
                        }
                        matches_probed
                    };
                    if owned {
                        this.clone_colliding_delegated_account(pubkey, slot)
                            .await;
                        return;
                    }
                    generation_probes = 0;
                    probed_slot = None;
                    continue;
                }

                if probed_slot == Some(slot) {
                    generation_probes += 1;
                } else {
                    generation_probes = 1;
                    probed_slot = Some(slot);
                }
                if generation_probes >= COLLISION_RECHECK_DELAYS.len() {
                    // Give up only on the generation that exhausted its
                    // probes; a newer one racing in keeps the worker going.
                    let dropped = {
                        let mut rechecks =
                            this.pending_collision_rechecks.lock();
                        let matches_probed = rechecks
                            .peek(&record_pda)
                            .is_some_and(|&(_, s)| s == slot);
                        if matches_probed {
                            rechecks.pop(&record_pda);
                        }
                        matches_probed
                    };
                    if dropped {
                        trace!(
                            pubkey = %pubkey,
                            slot,
                            "Dropping internal DLP program subscription update (no record after rechecks)"
                        );
                        return;
                    }
                    generation_probes = 0;
                    probed_slot = None;
                }
            }
        });
    }

    /// Discovery for an account whose app data collides with an internal
    /// DLP discriminator, proven delegated by its mirrored record:
    /// authority-gated like discovery, then cloned through the deduped
    /// on-demand path with a forced refresh.
    pub(super) async fn clone_colliding_delegated_account(
        &self,
        pubkey: Pubkey,
        slot: u64,
    ) {
        // A pre-delegation bank copy must be force-refreshed; only a
        // delegated copy at the update slot or newer settles the candidate.
        let fresh_delegated_in_bank = self
            .accounts_bank
            .get_account(&pubkey)
            .is_some_and(|in_bank| {
                in_bank.delegated() && in_bank.remote_slot() >= slot
            });
        if fresh_delegated_in_bank {
            return;
        }
        let fetch_context = AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
        );
        // Same authority gate as discovery: accounts delegated to another
        // validator must not be cloned from the firehose.
        let record_context = fetch_context
            .clone()
            .with_reason(AccountFetchReason::DelegationRecord);
        let companion_fetch_log_context = CompanionFetchLogContext {
            origin: record_context.clone(),
            primary_pubkey: pubkey,
            context_slot: slot,
        };
        let Some((deleg_record, _)) = self
            .fetch_and_parse_delegation_record(
                pubkey,
                slot,
                record_context,
                companion_fetch_log_context,
            )
            .await
        else {
            trace!(
                pubkey = %pubkey,
                slot = slot,
                "Colliding delegated account has no resolvable delegation record; falling back to on-demand cloning"
            );
            return;
        };
        let is_delegated_to_us = deleg_record.authority
            == self.validator_pubkey
            || deleg_record.authority == Pubkey::default();
        if !is_delegated_to_us {
            metrics::inc_discovered_dlp_update_delegated_elsewhere();
            trace!(
                pubkey = %pubkey,
                authority = %deleg_record.authority,
                "Ignoring colliding delegated account delegated elsewhere"
            );
            return;
        }
        // The deduped fetch can join an in-flight pre-delegation owner and
        // settle stale; retry until the bank reflects the mirrored delegation
        // (at the update slot itself only a delegated copy settles).
        const MAX_RELEASE_CLONE_ATTEMPTS: usize = 3;
        for _ in 0..MAX_RELEASE_CLONE_ATTEMPTS {
            // The update may predate a local undelegation that started in
            // the meantime. Do not let the mirrored record bypass the normal
            // confirmation check and overwrite protected local state.
            if self
                .accounts_bank
                .get_account(&pubkey)
                .is_some_and(|in_bank| {
                    in_bank.undelegating()
                        && account_still_undelegating_on_chain(
                            &pubkey,
                            true,
                            in_bank.remote_slot(),
                            Some(deleg_record),
                            &self.validator_pubkey,
                        )
                })
            {
                trace!(
                    pubkey = %pubkey,
                    slot = slot,
                    "Ignoring colliding delegated account while local undelegation remains pending"
                );
                return;
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
                Err(err) => {
                    warn!(
                        pubkey = %pubkey,
                        error = %err,
                        "Failed to clone colliding delegated account"
                    );
                    return;
                }
            };
            let unresolvable = result
                .not_found_on_chain
                .iter()
                .chain(result.missing_delegation_record.iter())
                .any(|(missing_pubkey, _)| missing_pubkey == &pubkey);
            if unresolvable {
                trace!(
                    pubkey = %pubkey,
                    slot = slot,
                    ?result,
                    "Colliding delegated account no longer resolvable on chain"
                );
                return;
            }
            let in_bank = self.accounts_bank.get_account(&pubkey);
            let settled = in_bank.as_ref().is_some_and(|in_bank| {
                in_bank.remote_slot() > slot
                    || (in_bank.remote_slot() == slot && in_bank.delegated())
            });
            if settled {
                return;
            }
            if in_bank.is_some_and(|in_bank| {
                in_bank.undelegating()
                    && deleg_record.delegation_slot <= in_bank.remote_slot()
            }) {
                break;
            }
        }
        // The RPC kept serving pre-delegation (or still-undelegating) state.
        // Requeue for strictly newer evidence — a fresh record update or the
        // mirror watermark passing the failed generation — so a later record
        // state re-triggers discovery instead of waiting for on-demand
        // access, as the replaced re-parking did.
        warn!(
            pubkey = %pubkey,
            slot,
            "Colliding delegated account did not settle at the update slot; requeueing for newer record evidence"
        );
        self.schedule_collision_recheck(pubkey, slot + 1);
    }

    pub(super) async fn maybe_greedily_clone_discovered_delegated_account(
        &self,
        pubkey: Pubkey,
        update: &ForwardedSubscriptionUpdate,
    ) -> bool {
        if self.accounts_bank.get_account(&pubkey).is_some() {
            return false;
        }

        let Some(account) = update.account.fresh_account() else {
            return false;
        };

        if !account.owner().eq(&dlp_api::id()) {
            return false;
        }

        let discovery_context = AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
        );
        let record_context = discovery_context
            .clone()
            .with_reason(AccountFetchReason::DelegationRecord);
        let companion_fetch_log_context = CompanionFetchLogContext {
            origin: record_context.clone(),
            primary_pubkey: pubkey,
            context_slot: account.remote_slot(),
        };

        let Some((deleg_record, delegation_actions)) = self
            .fetch_and_parse_delegation_record(
                pubkey,
                account.remote_slot(),
                record_context,
                companion_fetch_log_context,
            )
            .await
        else {
            trace!(
                pubkey = %pubkey,
                slot = account.remote_slot(),
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
        pubkeys_to_clone.extend(greedy_ata_pubkeys.iter().copied().filter(
            |ata_pubkey| self.accounts_bank.get_account(ata_pubkey).is_none(),
        ));

        // Keep eATA discovery with its candidate base ATAs in one clone batch
        // so the normal ATA projection path runs for the same update.
        let clone_result = if greedy_ata_pubkeys.is_empty() {
            self.fetch_and_clone_accounts_with_dedup(
                &pubkeys_to_clone,
                None,
                Some(account.remote_slot()),
                discovery_context.clone(),
            )
            .await
        } else {
            self.fetch_and_clone_accounts(
                &pubkeys_to_clone,
                None,
                Some(account.remote_slot()),
                discovery_context.clone(),
            )
            .await
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
                let bank_slot = self
                    .accounts_bank
                    .get_account(&pubkey)
                    .map(|in_bank| in_bank.remote_slot());
                if bank_slot.is_none_or(|slot| slot < account.remote_slot()) {
                    trace!(
                        pubkey = %pubkey,
                        bank_slot,
                        update_slot = account.remote_slot(),
                        ?result,
                        "Greedy clone did not materialize a fresh enough account; falling back"
                    );
                    false
                } else if let Some(projected_ata_clone_request) = self
                    .maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
                        pubkey,
                        &account,
                        update.source,
                        Some(&deleg_record),
                        &delegation_actions,
                        &CompanionFetchLogContext {
                            origin: discovery_context.clone(),
                            primary_pubkey: pubkey,
                            context_slot: account.remote_slot(),
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
                            slot = account.remote_slot(),
                            "Greedily cloned delegated account"
                        );
                        true
                    }
                } else {
                    let cloned_ata_pubkey = greedy_ata_pubkeys
                        .iter()
                        .copied()
                        .find(|ata_pubkey| {
                            self.accounts_bank
                                .get_account(ata_pubkey)
                                .is_some_and(|account_in_bank| {
                                    account_in_bank.remote_slot()
                                        >= account.remote_slot()
                                })
                        });
                    if let Some(ata_pubkey) = cloned_ata_pubkey {
                        trace!(
                            pubkey = %pubkey,
                            ata_pubkey = %ata_pubkey,
                            slot = account.remote_slot(),
                            "Greedily cloned delegated account"
                        );
                    } else {
                        trace!(
                            pubkey = %pubkey,
                            slot = account.remote_slot(),
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
        account: AccountSharedData,
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

    pub(super) async fn resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
        &self,
        update: ForwardedSubscriptionUpdate,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> (
        Option<AccountSharedData>,
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

        if let Some(account) = account.fresh_account() {
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
                    .task_to_fetch_with_delegation_record(
                        pubkey,
                        ResolvedAccountSharedData::Fresh(account.clone()),
                        account.remote_slot(),
                        account.remote_slot(),
                        AccountFetchContext::subscription_update(
                            AccountFetchReason::DelegationRecord,
                        ),
                    )
                    .await
                {
                    Ok(Ok(AccountWithCompanion {
                        pubkey,
                        mut account,
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
                                    delegation_record.data(),
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
                                        slot = account.remote_slot(),
                                        owner = %delegation_record.owner,
                                        deleg_record = %delegation_record_display,
                                        "Resolving delegated account"
                                    );
                                }

                                self.apply_delegation_record_to_account(
                                    pubkey,
                                    &mut account,
                                    &delegation_record,
                                );

                                // For accounts delegated to us, subscribe to the original owner
                                // program for undelegation update resilience.
                                if account.delegated()
                                    && !self
                                        .programs_not_to_subscribe
                                        .contains(&delegation_record.owner)
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
                                    Some(account.into_account_shared_data()),
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
                                account.data(),
                            )
                        {
                            let observed = ObservedUndelegationRequest {
                                request_pda: pubkey,
                                delegated_account: request.delegated_account,
                                expires_at_slot: request.expires_at_slot,
                                observed_slot: account.remote_slot(),
                            };
                            trace!(
                                request_pda = %observed.request_pda,
                                delegated_account = %observed.delegated_account,
                                expires_at_slot = observed.expires_at_slot,
                                "Observed DLP undelegation request"
                            );
                            if let Err(broadcast::error::SendError(observed)) =
                                self.undelegation_request_sender.send(observed)
                            {
                                warn!(
                                    request_pda = %observed.request_pda,
                                    delegated_account = %observed.delegated_account,
                                    observed_slot = observed.observed_slot,
                                    expires_at_slot = observed.expires_at_slot,
                                    drop_reason = "no_active_subscribers",
                                    "Dropped observed DLP undelegation request because no subscribers are active"
                                );
                            }
                            (
                                Some(account.into_account_shared_data()),
                                None,
                                DelegationActions::default(),
                            )
                        } else if is_internal_dlp_account_data(account.data()) {
                            (
                                Some(account.into_account_shared_data()),
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
                        account,
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
            error!(pubkey = %pubkey, account = ?account, "BUG: Received subscription update without fresh account");
            (None, None, DelegationActions::default())
        }
    }

    pub(super) async fn maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
        &self,
        eata_pubkey: Pubkey,
        eata_account: &AccountSharedData,
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

    #[cfg(test)]
    pub(super) fn is_known_empty_eata(&self, eata_pubkey: &Pubkey) -> bool {
        ata_projection::is_known_empty_eata(self, eata_pubkey)
    }

    pub(super) async fn maybe_project_ata_from_subscription_update(
        &self,
        ata_pubkey: Pubkey,
        ata_account: AccountSharedData,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> (
        AccountSharedData,
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

    /// Parses a delegation record from account data bytes.
    /// Returns the parsed DelegationRecord, or InvalidDelegationRecord error
    /// if parsing fails.
    pub(super) fn parse_delegation_record(
        &self,
        data: &[u8],
        delegation_record_pubkey: Pubkey,
    ) -> ChainlinkResult<(DelegationRecord, Option<DelegationActions>)> {
        delegation::parse_delegation_record(
            data,
            delegation_record_pubkey,
            self.validator_keypair.as_ref(),
        )
    }

    /// Applies delegation record settings to an account: sets the owner,
    /// delegation status, and confined status based on the delegation
    /// record's authority field.
    /// Returns commit frequency if account is delegated to us
    pub(super) fn apply_delegation_record_to_account(
        &self,
        account_pubkey: Pubkey,
        account: &mut ResolvedAccountSharedData,
        delegation_record: &DelegationRecord,
    ) -> Option<u64> {
        delegation::apply_delegation_record_to_account(
            self,
            account_pubkey,
            account,
            delegation_record,
        )
    }

    /// Returns the pubkey of another validator if account is delegated to them,
    /// None if delegated to us or delegated to the system program (confined).
    pub(super) fn get_delegated_to_other(
        &self,
        delegation_record: &DelegationRecord,
    ) -> Option<Pubkey> {
        delegation::get_delegated_to_other(self, delegation_record)
    }

    /// Fetches and parses the delegation record for an account, returning the
    /// parsed DelegationRecord if found and valid, None otherwise.
    pub(super) async fn fetch_and_parse_delegation_record(
        &self,
        account_pubkey: Pubkey,
        min_context_slot: u64,
        fetch_context: metrics::AccountFetchContext,
        companion_fetch_log_context: CompanionFetchLogContext,
    ) -> Option<(DelegationRecord, Option<DelegationActions>)> {
        delegation::fetch_and_parse_delegation_record(
            self,
            account_pubkey,
            min_context_slot,
            fetch_context,
            &companion_fetch_log_context,
        )
        .await
    }
}
