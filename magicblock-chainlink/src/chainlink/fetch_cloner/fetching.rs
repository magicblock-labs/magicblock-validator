//! Fetch orchestration: fetch/clone pipelines, dedup, task builders.

use std::{collections::HashSet, sync::atomic::Ordering, time::Duration};

use dlp_api::pda::delegation_record_pda_from_delegated_account;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::EATA_PROGRAM_ID;
use magicblock_metrics::metrics::{
    self, AccountFetchContext, AccountFetchReason, BankPrecheckOutcome,
    BankPrecheckReason, ChainlinkCloneIntent, ChainlinkCloneOutcome,
    ChainlinkCloneRemoteResult, ChainlinkCompanionFetchKind,
};
use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use tokio::{sync::mpsc, task, task::JoinSet};
use tracing::*;

use super::*;

impl<T, U, V, C> FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    /// Tries to fetch all accounts in `pubkeys` and clone them into the bank.
    /// If `mark_empty` is provided, accounts in that list that are
    /// not found on chain will be added with zero lamports to the bank.
    ///
    /// - **pubkeys**: list of accounts to fetch and clone
    /// - **mark_empty**: optional list of accounts that should be added as empty if not found on
    ///   chain
    /// - **slot**: optional slot to use as minimum context slot for the accounts being cloned
    ///
    /// NOTE: accounts fetched here have not been found in the bank
    pub(super) async fn fetch_and_clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let accs = match self
            .fetch_accounts(
                pubkeys,
                mark_empty_if_not_found,
                slot,
                fetch_context.clone(),
            )
            .await
        {
            Ok(accs) => accs,
            Err(err) => {
                for _ in pubkeys {
                    metrics::inc_chainlink_clone_accounts_total_with_context(
                        fetch_context.clone(),
                        ChainlinkCloneRemoteResult::Failed,
                        ChainlinkCloneIntent::Unknown,
                        ChainlinkCloneOutcome::Skipped,
                    );
                }
                return Err(err);
            }
        };
        self.clone_accounts(
            pubkeys,
            accs,
            mark_empty_if_not_found,
            slot,
            fetch_context,
        )
        .await
    }

    #[instrument(skip(self, pubkeys, mark_empty_if_not_found), fields(tx_sig = tracing::field::Empty))]
    pub(super) async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<Vec<RemoteAccount>> {
        if let Some(sig) = fetch_context.signature() {
            tracing::Span::current().record("tx_sig", sig.to_string());
        }
        if tracing::enabled!(tracing::Level::TRACE) {
            let pubkeys_count = pubkeys.len();
            trace!(count = pubkeys_count, "Fetching accounts");
        }

        // Increment fetch counter for testing deduplication (count per account being fetched)
        self.fetch_count
            .fetch_add(pubkeys.len() as u64, Ordering::Relaxed);

        // Keep the main account fetch aligned with the freshest observed slot.
        let min_context_slot = slot.map(|subscription_slot| {
            subscription_slot.max(self.remote_account_provider.chain_slot())
        });

        let accs = self
            .remote_account_provider
            .try_get_multi(
                pubkeys,
                mark_empty_if_not_found,
                fetch_context,
                min_context_slot,
            )
            .await?;

        if tracing::enabled!(tracing::Level::TRACE) {
            let accs_count = accs.len();
            trace!(count = accs_count, "Fetched accounts");
        }
        Ok(accs)
    }

    #[instrument(skip(self, pubkeys, accs, mark_empty_if_not_found), fields(tx_sig = tracing::field::Empty))]
    pub(super) async fn clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        accs: Vec<RemoteAccount>,
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if let Some(sig) = fetch_context.signature() {
            tracing::Span::current().record("tx_sig", sig.to_string());
        }

        // Keep resolution fetches aligned with the freshest observed slot.
        let min_context_slot = slot.map(|subscription_slot| {
            subscription_slot.max(self.remote_account_provider.chain_slot())
        });

        let ClassifiedAccounts {
            not_found,
            plain,
            owned_by_deleg,
            programs,
            atas,
        } = pipeline::classify_remote_accounts(accs, pubkeys);

        if tracing::enabled!(tracing::Level::TRACE) {
            let not_found = not_found
                .iter()
                .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let plain = plain
                .iter()
                .map(|p| p.pubkey.to_string())
                .collect::<Vec<_>>();
            let owned_by_deleg = owned_by_deleg
                .iter()
                .map(|(pubkey, _, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let programs = programs
                .iter()
                .map(|(p, _, _)| p.to_string())
                .collect::<Vec<_>>();
            let atas = atas
                .iter()
                .map(|(a, _, _, _)| a.to_string())
                .collect::<Vec<_>>();
            trace!(
                "Fetched accounts: \nnot_found:      {not_found:?} \nplain:          {plain:?} \nowned_by_deleg: {owned_by_deleg:?}\nprograms:       {programs:?} \natas:       {atas:?}",
            );
        }

        let PartitionedNotFound {
            clone_as_empty,
            not_found,
        } = pipeline::partition_not_found(mark_empty_if_not_found, not_found);

        // For accounts we couldn't find we cannot do anything. We will let code depending
        // on them to be in the bank fail on its own
        if !not_found.is_empty() {
            trace!(
                "Could not find accounts on chain: {:?}",
                not_found
                    .iter()
                    .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                    .collect::<Vec<_>>()
            );
        }

        // We mark some accounts as empty if we know that they will never exist on chain
        if tracing::enabled!(tracing::Level::TRACE)
            && !clone_as_empty.is_empty()
        {
            trace!(
                "Cloning accounts as empty: {:?}",
                clone_as_empty
                    .iter()
                    .map(|(p, _)| p.to_string())
                    .collect::<Vec<_>>()
            );
        }

        // For potentially delegated accounts we update the owner and delegation state first
        let ResolvedDelegatedAccounts {
            mut accounts_to_clone,
            mut record_subs,
            missing_delegation_record,
        } = match pipeline::resolve_delegated_accounts(
            self,
            owned_by_deleg,
            plain,
            min_context_slot,
            fetch_context.clone(),
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(err) => {
                release_subs(
                    &self.remote_account_provider,
                    pubkeys.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        }
                    }),
                )
                .await;
                return Err(err);
            }
        };

        let ResolvedPrograms {
            loaded_programs,
            mut program_data_subs,
        } = match pipeline::resolve_programs_with_program_data(
            self,
            programs,
            min_context_slot,
            fetch_context.clone(),
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(err) => {
                let releases = pubkeys
                    .iter()
                    .copied()
                    .map(|pubkey| SubscriptionRelease::Pubkey {
                        pubkey,
                        reason: SubscriptionReason::DirectAccount,
                    })
                    .chain(record_subs.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        }
                    }))
                    .chain(record_subs.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DelegationRecord,
                        }
                    }))
                    .collect::<Vec<_>>();
                release_subs(&self.remote_account_provider, releases).await;
                return Err(err);
            }
        };

        let mut loaded_programs = loaded_programs;
        let mut all_requested_pubkeys = pubkeys.to_vec();
        all_requested_pubkeys.extend(record_subs.iter().copied());
        all_requested_pubkeys.extend(program_data_subs.iter().copied());

        // We will compute subscription cancellations after ATA handling, once accounts_to_clone is finalized

        // Handle ATAs: for each detected ATA, we derive the eATA PDA, subscribe to both,
        // and, if the ATA is delegated to us and the eATA exists, we clone the eATA data
        // into the ATA in the bank.
        // eATA subscriptions are kept implicitly (not tracked for release).
        let ata_accounts = ata_projection::resolve_ata_with_eata_projection(
            self,
            atas,
            min_context_slot,
            fetch_context.clone(),
        )
        .await;
        accounts_to_clone.extend(ata_accounts);

        // Ensure all accounts referenced by delegation actions exist and are
        // cloned before we execute those actions as part of account cloning.
        let action_dependencies =
            pipeline::collect_delegation_action_dependencies(
                &accounts_to_clone,
            );
        let action_dependencies_to_fetch = action_dependencies
            .into_iter()
            .filter(|dependency| {
                self.accounts_bank.get_account(dependency).is_none()
                    && !accounts_to_clone
                        .iter()
                        .any(|request| request.pubkey.eq(dependency))
                    && !loaded_programs
                        .iter()
                        .any(|program| program.program_id.eq(dependency))
            })
            .collect::<Vec<_>>();

        if !action_dependencies_to_fetch.is_empty() {
            if tracing::enabled!(tracing::Level::TRACE) {
                trace!(
                    dependencies = ?action_dependencies_to_fetch,
                    "Ensuring delegation action dependencies"
                );
            }

            self.fetch_count.fetch_add(
                action_dependencies_to_fetch.len() as u64,
                Ordering::Relaxed,
            );
            let action_dependency_context = fetch_context
                .clone()
                .with_reason(AccountFetchReason::ActionDependencyMissing);
            let action_dep_accs = self
                .remote_account_provider
                .try_get_multi(
                    &action_dependencies_to_fetch,
                    None,
                    action_dependency_context.clone(),
                    min_context_slot,
                )
                .await?;
            all_requested_pubkeys
                .extend(action_dependencies_to_fetch.iter().copied());

            let ClassifiedAccounts {
                not_found,
                plain,
                owned_by_deleg,
                programs,
                atas,
            } = pipeline::classify_remote_accounts(
                action_dep_accs,
                &action_dependencies_to_fetch,
            );

            if tracing::enabled!(tracing::Level::TRACE) && !not_found.is_empty()
            {
                trace!(
                    dependencies = ?not_found,
                    "Delegation action dependencies not found on chain; continuing clone flow"
                );
            }

            let ResolvedDelegatedAccounts {
                accounts_to_clone: action_dep_accounts_to_clone,
                record_subs: action_dep_record_subs,
                missing_delegation_record: action_dep_missing_delegation_record,
            } = match pipeline::resolve_delegated_accounts(
                self,
                owned_by_deleg,
                plain,
                min_context_slot,
                action_dependency_context.clone(),
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        &accounts_to_clone,
                        &loaded_programs,
                        record_subs.clone(),
                        program_data_subs.clone(),
                    );
                    release_subs(&self.remote_account_provider, releases).await;
                    return Err(err);
                }
            };

            if !action_dep_missing_delegation_record.is_empty() {
                let releases = pipeline::compute_subscription_releases(
                    &all_requested_pubkeys,
                    &accounts_to_clone,
                    &loaded_programs,
                    record_subs
                        .iter()
                        .copied()
                        .chain(action_dep_record_subs.iter().copied())
                        .collect(),
                    program_data_subs.clone(),
                );
                release_subs(&self.remote_account_provider, releases).await;
                return Err(ChainlinkError::MissingDelegationActionAccounts(
                    action_dep_missing_delegation_record
                        .iter()
                        .map(|(pubkey, _)| *pubkey)
                        .collect(),
                ));
            }

            all_requested_pubkeys
                .extend(action_dep_record_subs.iter().copied());
            record_subs.extend(action_dep_record_subs);

            let ResolvedPrograms {
                loaded_programs: action_dep_loaded_programs,
                program_data_subs: action_dep_program_data_subs,
            } = match pipeline::resolve_programs_with_program_data(
                self,
                programs,
                min_context_slot,
                action_dependency_context.clone(),
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let mut cleanup_accounts_to_clone =
                        accounts_to_clone.clone();
                    cleanup_accounts_to_clone
                        .extend(action_dep_accounts_to_clone.clone());
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        &cleanup_accounts_to_clone,
                        &loaded_programs,
                        record_subs.clone(),
                        program_data_subs.clone(),
                    );
                    release_subs(&self.remote_account_provider, releases).await;
                    return Err(err);
                }
            };

            all_requested_pubkeys
                .extend(action_dep_program_data_subs.iter().copied());
            program_data_subs.extend(action_dep_program_data_subs);

            let action_dep_ata_accounts =
                ata_projection::resolve_ata_with_eata_projection(
                    self,
                    atas,
                    min_context_slot,
                    action_dependency_context,
                )
                .await;

            accounts_to_clone.extend(action_dep_accounts_to_clone);
            accounts_to_clone.extend(action_dep_ata_accounts);
            loaded_programs.extend(action_dep_loaded_programs);
        }

        let releases = pipeline::compute_subscription_releases(
            &all_requested_pubkeys,
            &accounts_to_clone,
            &loaded_programs,
            record_subs,
            program_data_subs,
        );

        pipeline::clone_accounts_and_programs(
            self,
            accounts_to_clone,
            loaded_programs,
            fetch_context,
        )
        .await?;

        release_subs(&self.remote_account_provider, releases).await;

        Ok(FetchAndCloneResult {
            not_found_on_chain: not_found,
            missing_delegation_record,
        })
    }

    /// Determines if the account finished undelegating on chain.
    /// If it has finished undelegating, we should refresh it in the bank.
    /// - **pubkey**: the account pubkey
    /// - **in_bank**: the account as it exists in the bank
    ///
    /// Returns true if the account should be refreshed in the bank
    pub(super) async fn should_refresh_undelegating_in_bank_account(
        &self,
        pubkey: &Pubkey,
        in_bank: &AccountSharedData,
        fetch_context: AccountFetchContext,
    ) -> RefreshDecision {
        if in_bank.undelegating() {
            debug!(
                pubkey = %pubkey,
                delegated = in_bank.delegated(),
                undelegating = in_bank.undelegating(),
                "Fetching undelegating account"
            );

            if let Some(eata_pubkey) =
                ata_projection::derive_eata_pubkey_from_ata_layout(
                    pubkey, in_bank,
                )
            {
                let undelegating_refresh_context = fetch_context
                    .clone()
                    .with_reason(AccountFetchReason::UndelegatingRefresh);
                let companion_fetch_log_context = CompanionFetchLogContext {
                    origin: undelegating_refresh_context.clone(),
                    primary_pubkey: eata_pubkey,
                    context_slot: self.remote_account_provider.chain_slot(),
                };
                let projected_deleg_record = self
                    .fetch_and_parse_delegation_record(
                        eata_pubkey,
                        self.remote_account_provider.chain_slot(),
                        undelegating_refresh_context,
                        companion_fetch_log_context,
                    )
                    .await;
                if projected_deleg_record.as_ref().is_some_and(|(record, _)| {
                    record.owner == EATA_PROGRAM_ID
                        && record.authority == self.validator_pubkey
                }) {
                    debug!(
                        pubkey = %pubkey,
                        eata_pubkey = %eata_pubkey,
                        "Keeping undelegating ATA in bank while companion eATA remains delegated"
                    );
                    return RefreshDecision::No;
                }
            }

            let undelegating_refresh_context = fetch_context
                .clone()
                .with_reason(AccountFetchReason::UndelegatingRefresh);
            let companion_fetch_log_context = CompanionFetchLogContext {
                origin: undelegating_refresh_context.clone(),
                primary_pubkey: *pubkey,
                context_slot: self.remote_account_provider.chain_slot(),
            };
            let deleg_record = self
                .fetch_and_parse_delegation_record(
                    *pubkey,
                    self.remote_account_provider.chain_slot(),
                    undelegating_refresh_context,
                    companion_fetch_log_context,
                )
                .await;

            if deleg_record.is_none() {
                // If there is no delegation record then it is possible that the account itself
                // does not exist either.
                // In that case we need to refresh it as empty to clear the undelegation state.
                self.invalidate_mirrored_record(pubkey);
                return RefreshDecision::YesAndMarkEmptyIfNotFound;
            }

            let delegated_on_chain =
                deleg_record.as_ref().is_some_and(|(dr, _)| {
                    dr.authority.eq(&self.validator_pubkey)
                        || dr.authority.eq(&Pubkey::default())
                });
            let deleg_record = deleg_record.map(|el| el.0);
            if !account_still_undelegating_on_chain(
                pubkey,
                delegated_on_chain,
                in_bank.remote_slot(),
                deleg_record,
                &self.validator_pubkey,
            ) {
                debug!(
                    "Account {pubkey} marked as undelegating will be overridden since undelegation completed"
                );
                self.invalidate_mirrored_record(pubkey);
                return RefreshDecision::Yes;
            }
        } else if in_bank.owner().eq(&dlp_api::id()) {
            debug!(
                "Account {pubkey} owned by deleg program not marked as undelegating"
            );
        }
        RefreshDecision::No
    }

    /// Fetch and clone accounts with request deduplication to avoid parallel fetches of the same account.
    /// This method implements the new logic where:
    /// 1. Check synchronously if account is in bank, return immediately if found
    /// 2. If account is pending, add to pending requests and await
    /// 3. Create pending entries and fetch via RemoteAccountProvider
    /// 4. Once fetched, clone into bank and respond to all pending requests
    /// 5. Clear pending requests for that account
    ///
    /// Note: since we fetch each account only once in parallel, we also avoid fetching
    /// the same delegation record in parallel.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found))]
    pub async fn fetch_and_clone_accounts_with_dedup(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        self.fetch_and_clone_accounts_with_dedup_forced_refresh(
            pubkeys,
            mark_empty_if_not_found,
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
        // We cannot clone blacklisted accounts, thus either they are already
        // in the bank (e.g. native programs) or they don't exist and the transaction
        // will fail later
        let mut pubkeys = pubkeys
            .iter()
            .filter(|p| !self.blacklisted_accounts.contains(p))
            .collect::<Vec<_>>();
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching and cloning accounts with dedup");
        }

        let mut in_bank = HashSet::new();
        let mut extra_mark_empty = vec![];
        let mut bank_hit_no_fetch_non_undelegating_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_still_valid_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_timeout_count = 0_u64;
        let mut bank_hit_undelegating_refresh_required_count = 0_u64;
        let mut bank_miss_remote_required_count = 0_u64;
        let mut forced_refresh_remote_required_count = 0_u64;

        // Phase 1: Sync bank check — separate undelegating accounts
        // (which need async RPC) from non-undelegating (handled
        // synchronously). A forced refresh may replace a locally undelegating
        // account only when its caller supplies a matching newer delegation
        // generation. Action dependencies never do; the locked runtime guard
        // rejects the action if that dependency remains unsafe.
        let mut undelegating_checks: Vec<(Pubkey, AccountSharedData)> = vec![];
        for pubkey in pubkeys.iter() {
            if force_refresh_pubkeys.contains(*pubkey) {
                if let Some(account_in_bank) =
                    self.accounts_bank.get_account(pubkey)
                {
                    if account_in_bank.undelegating() {
                        let can_replace_undelegating = confirmed_redelegation
                            .is_some_and(
                                |(confirmed_pubkey, delegation_slot)| {
                                    confirmed_pubkey == **pubkey
                                        && delegation_slot
                                            > account_in_bank.remote_slot()
                                },
                            );
                        if !can_replace_undelegating {
                            bank_hit_no_fetch_undelegating_still_valid_count +=
                                1;
                            in_bank.insert(**pubkey);
                            continue;
                        }
                    }
                }
                forced_refresh_remote_required_count += 1;
                continue;
            }
            if let Some(account_in_bank) =
                self.accounts_bank.get_account(pubkey)
            {
                if account_in_bank.undelegating() {
                    undelegating_checks.push((**pubkey, account_in_bank));
                } else {
                    if account_in_bank.owner().eq(&dlp_api::id()) {
                        debug!(
                            pubkey = %pubkey,
                            "Account owned by deleg program not marked as undelegating"
                        );
                    }
                    if tracing::enabled!(tracing::Level::TRACE) {
                        let delegated = account_in_bank.delegated();
                        let owner = account_in_bank.owner();
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
            } else {
                bank_miss_remote_required_count += 1;
            }
        }

        // Phase 2: Parallel undelegation checks via JoinSet
        if !undelegating_checks.is_empty() {
            let mut join_set = JoinSet::new();
            for (pubkey, account_in_bank) in undelegating_checks {
                let this = self.clone();
                let fetch_context = fetch_context.clone();
                join_set.spawn(async move {
                    let decision = match tokio::time::timeout(
                        Duration::from_secs(5),
                        this.should_refresh_undelegating_in_bank_account(
                            &pubkey,
                            &account_in_bank,
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

        let mut mark_empty_set = mark_empty_if_not_found
            .unwrap_or(&[])
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        mark_empty_set.extend(extra_mark_empty);

        let mut waiters: Vec<PendingWaiter> = vec![];
        let mut claimed_ops: Vec<ClaimedOperation> = vec![];
        for pubkey in pubkeys {
            match self
                .claim_or_join_owned_operation(*pubkey, fetch_context.clone())
            {
                PendingClaim::Created(handles) => {
                    let PendingHandles {
                        waiter,
                        deadline,
                        cancel,
                        owner,
                    } = handles;
                    let waiter_pubkey = waiter.pubkey();
                    let Some(owner) = owner else {
                        cancel.notify_waiters();
                        finish_pending(
                            &self.pending_requests,
                            waiter_pubkey,
                            waiter.generation(),
                            PendingTerminal::Failed(PendingFailure::Cancelled),
                        );
                        return Err(
                            ChainlinkError::MissingPendingRequestOwner(
                                waiter_pubkey,
                            ),
                        );
                    };
                    claimed_ops.push(ClaimedOperation {
                        pubkey: waiter_pubkey,
                        generation: waiter.generation(),
                        deadline,
                        cancel,
                        owner,
                    });
                    waiters.push(waiter);
                }
                PendingClaim::Joined(handles) => waiters.push(handles.waiter),
            }
        }
        if !claimed_ops.is_empty() {
            self.spawn_batched_owned_operation(
                claimed_ops,
                &mark_empty_set,
                slot,
                fetch_context.clone(),
            );
        }

        let mut final_result = FetchAndCloneResult {
            not_found_on_chain: vec![],
            missing_delegation_record: vec![],
        };
        for waiter in waiters {
            let pubkey = waiter.pubkey();
            match waiter.wait().await? {
                PendingTerminal::Success(owner_result) => {
                    for entry in owner_result.not_found_on_chain {
                        if entry.0 == pubkey {
                            final_result.not_found_on_chain.push(entry);
                        }
                    }
                    for entry in owner_result.missing_delegation_record {
                        if entry.0 == pubkey {
                            final_result.missing_delegation_record.push(entry);
                        }
                    }
                }
                PendingTerminal::Failed(failure) => {
                    return Err(failure.into_chainlink_error(pubkey));
                }
            }
        }

        Ok(final_result)
    }

    /// Resolves an account with its delegation record, pairing the account
    /// copy the caller already holds (at `account_slot`) with a mirrored
    /// record when the pair is snapshot-consistent: the record must be
    /// proven through `slot` AND unchanged since at or before
    /// `account_slot`, so both sides describe the same chain state. That
    /// path performs no RPC at all. Every other outcome runs the batched
    /// two-account fetch, byte-for-byte the prior path.
    pub(super) fn task_to_fetch_with_delegation_record(
        &self,
        pubkey: Pubkey,
        account: ResolvedAccountSharedData,
        account_slot: u64,
        slot: u64,
        fetch_context: AccountFetchContext,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(&pubkey);

        if let Some(MirrorLookup::Hit {
            data,
            slot: record_slot,
        }) = self
            .record_mirror
            .as_ref()
            .map(|mirror| mirror.get(&delegation_record_pubkey, slot))
        {
            use metrics::RecordMirrorLookupOutcome as Outcome;
            if record_slot > account_slot {
                // The record changed after the held account copy was taken;
                // only the batched fetch can produce a consistent pair.
                metrics::inc_record_mirror_lookup(Outcome::Stale);
            } else if !is_delegation_record_data(&data) {
                metrics::inc_record_mirror_lookup(Outcome::ParseFallback);
            } else {
                metrics::inc_record_mirror_lookup(Outcome::Hit);
                trace!(
                    pubkey = %pubkey,
                    companion = %delegation_record_pubkey,
                    slot,
                    "Pairing held account with mirrored delegation record"
                );
                let companion_account = ResolvedAccountSharedData::Fresh(
                    AccountSharedData::from(Account {
                        lamports: MIRRORED_RECORD_LAMPORTS,
                        data,
                        owner: dlp_api::id(),
                        executable: false,
                        rent_epoch: 0,
                    }),
                );
                return task::spawn(async move {
                    Ok(AccountWithCompanion {
                        pubkey,
                        account,
                        companion_pubkey: delegation_record_pubkey,
                        companion_account: Some(companion_account),
                    })
                });
            }
        }

        self.task_to_fetch_with_companion(
            pubkey,
            delegation_record_pubkey,
            slot,
            fetch_context.with_reason(AccountFetchReason::DelegationRecord),
            ChainlinkCompanionFetchKind::DelegationRecord,
        )
    }

    pub(super) fn task_to_fetch_with_program_data(
        &self,
        pubkey: Pubkey,
        slot: u64,
        fetch_context: AccountFetchContext,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let program_data_pubkey =
            get_loaderv3_get_program_data_address(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            program_data_pubkey,
            slot,
            fetch_context.with_reason(AccountFetchReason::ProgramData),
            ChainlinkCompanionFetchKind::ProgramData,
        )
    }

    pub(super) fn task_to_fetch_with_companion(
        &self,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        slot: u64,
        fetch_context: AccountFetchContext,
        companion_fetch_kind: ChainlinkCompanionFetchKind,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let provider = self.remote_account_provider.clone();
        let bank = self.accounts_bank.clone();
        let fetch_count = self.fetch_count.clone();
        task::spawn(async move {
            trace!(
                pubkey = %pubkey,
                companion = %companion_pubkey,
                slot,
                "Fetching account with companion"
            );

            // Increment fetch counter for testing deduplication (2 accounts: pubkey + delegation_record_pubkey)
            fetch_count.fetch_add(2, Ordering::Relaxed);

            provider
                .try_get_multi_until_slots_match(
                    &[pubkey, companion_pubkey],
                    Some(MatchSlotsConfig {
                        min_context_slot: Some(slot),
                        ..MatchSlotsConfig::new(companion_fetch_kind)
                    }),
                    fetch_context,
                )
                .await
                .map_err(ChainlinkError::from)
                .and_then(|accs| {
                    match accs.as_slice() {
                        [acc_first, acc_last] => {
                            Ok((acc_first.clone(), acc_last.clone()))
                        }
                        _ => Err(ChainlinkError::UnexpectedAccountCount(format!(
                            "Expected exactly 2 accounts for pubkey {} and companion {}, got {}",
                            pubkey,
                            companion_pubkey,
                            accs.len()
                        ))),
                    }
                })
                .and_then(|(acc, deleg)| {
                    Self::resolve_account_with_companion(
                        &bank,
                        pubkey,
                        companion_pubkey,
                        acc,
                        deleg,
                    )
                })
        })
    }

    pub(super) fn resolve_account_with_companion(
        bank: &V,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        acc: RemoteAccount,
        companion: RemoteAccount,
    ) -> ChainlinkResult<AccountWithCompanion> {
        use RemoteAccount::*;
        match (acc, companion) {
            // Account not found even though we found it previously - this is invalid,
            // either way we cannot use it now
            (NotFound(_), NotFound(_)) | (NotFound(_), Found(_)) => {
                Err(ChainlinkError::ResolvedAccountCouldNoLongerBeFound(pubkey))
            }
            (Found(acc), NotFound(_)) => {
                // Only account found without a companion
                // In case of delegation record fetch the account is either invalid
                // or a delegation record itself.
                // Clone it as is (without changing the owner or flagging as delegated)
                match acc.account.resolved_account_shared_data(bank) {
                    Some(account) => Ok(AccountWithCompanion {
                        pubkey,
                        account,
                        companion_pubkey,
                        companion_account: None,
                    }),
                    None => Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    ),
                }
            }
            (Found(acc), Found(comp)) => {
                // Found the delegation record, we include it so that the caller can
                // use it to add metadata to the account and use it for decision making
                let Some(comp_account) =
                    comp.account.resolved_account_shared_data(bank)
                else {
                    return Err(
                        ChainlinkError::ResolvedCompanionAccountCouldNoLongerBeFound(
                            companion_pubkey,
                        ),
                    );
                };
                let Some(account) =
                    acc.account.resolved_account_shared_data(bank)
                else {
                    return Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    );
                };
                Ok(AccountWithCompanion {
                    pubkey,
                    account,
                    companion_pubkey,
                    companion_account: Some(comp_account),
                })
            }
        }
    }

    /// Check if an account is currently being watched (subscribed to) by the
    /// remote account provider
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.remote_account_provider.is_watching(pubkey)
    }

    /// Subscribe to updates for a specific account
    /// This is typically used when an account is about to be undelegated
    /// and we need to start watching for changes
    #[instrument(skip(self))]
    pub(crate) async fn acquire_subscription_reason(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> ChainlinkResult<()> {
        self.remote_account_provider
            .acquire_subscription(pubkey, reason)
            .await
            .map_err(|err| {
                ChainlinkError::FailedToSubscribeToAccount(*pubkey, err)
            })
    }

    pub(crate) async fn ensure_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> ChainlinkResult<()> {
        self.remote_account_provider
            .ensure_subscription(pubkey, reason)
            .await
            .map_err(|err| {
                ChainlinkError::FailedToSubscribeToAccount(*pubkey, err)
            })
    }

    #[instrument(skip(self))]
    pub async fn subscribe_to_account_to_track_undelegation(
        &self,
        pubkey: &Pubkey,
    ) -> ChainlinkResult<()> {
        trace!(
            pubkey = %pubkey,
            reason = ?SubscriptionReason::UndelegationTracking,
            "Subscribing to account"
        );
        // Acquire undelegation tracking ownership before/with local
        // undelegating visibility; any LRU entry created for this reason is
        // protected from capacity eviction by the provider's bank-state
        // predicate and ownership filter.
        self.acquire_subscription_reason(
            pubkey,
            SubscriptionReason::UndelegationTracking,
        )
        .await
    }

    pub fn chain_slot(&self) -> u64 {
        self.remote_account_provider.chain_slot()
    }

    pub fn received_updates_count(&self) -> u64 {
        self.remote_account_provider.received_updates_count()
    }

    pub(crate) fn promote_accounts(&self, pubkeys: &[&Pubkey]) {
        self.remote_account_provider.promote_accounts(pubkeys);
    }

    pub fn try_get_removed_account_rx(
        &self,
    ) -> ChainlinkResult<mpsc::Receiver<Pubkey>> {
        Ok(self.remote_account_provider.try_get_removed_account_rx()?)
    }

    /// Best-effort airdrop helper: if the account doesn't exist in the bank or has 0 lamports,
    /// create/overwrite it as a plain system account with the provided lamports using the cloner path.
    #[instrument(skip(self))]
    pub async fn airdrop_account_if_empty(
        &self,
        pubkey: Pubkey,
        lamports: u64,
    ) -> ClonerResult<()> {
        if lamports == 0 {
            return Ok(());
        }
        let remote_slot =
            if let Some(acc) = self.accounts_bank.get_account(&pubkey) {
                if acc.lamports() > 0 {
                    return Ok(());
                }
                acc.remote_slot()
                    .max(self.remote_account_provider.chain_slot())
            } else {
                self.remote_account_provider.chain_slot()
            };
        // Build a plain system account with the requested balance
        let mut account =
            AccountSharedData::new(lamports, 0, &system_program::id());
        account.set_remote_slot(remote_slot);
        debug!(
            pubkey = %pubkey,
            lamports,
            remote_slot,
            "Auto-airdropping account"
        );
        let _sig = self
            .cloner
            .clone_account(AccountCloneRequest {
                pubkey,
                account,
                commit_frequency_ms: None,
                delegation_actions: DelegationActions::default(),
                delegated_to_other: None,
                needs_undelegation: false,
            })
            .await?;
        Ok(())
    }
}
