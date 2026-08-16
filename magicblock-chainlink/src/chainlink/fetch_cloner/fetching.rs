use super::*;

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    /// Parses a delegation record from account data bytes.
    /// Returns the parsed record or an invalid-record error.
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
        account: AccountBuilder,
        delegation_record: &DelegationRecord,
    ) -> (AccountBuilder, Option<u64>) {
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

    pub(super) async fn fetch_and_clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneBatchResult> {
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

    #[instrument(skip(self, pubkeys), fields(tx_sig = tracing::field::Empty))]
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
}

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    #[instrument(skip(self, pubkeys, accs), fields(tx_sig = tracing::field::Empty))]
    pub(super) async fn clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        accs: Vec<RemoteAccount>,
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneBatchResult> {
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
        let action_dependencies_to_fetch = {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            action_dependencies
                .into_iter()
                .filter(|dependency| {
                    !loader.contains(dependency).unwrap_or(false)
                        && !accounts_to_clone
                            .iter()
                            .any(|request| request.pubkey.eq(dependency))
                        && !loaded_programs
                            .iter()
                            .any(|program| program.program_id.eq(dependency))
                })
                .collect::<Vec<_>>()
        };

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

        let materialized = pipeline::clone_accounts_and_programs(
            self,
            accounts_to_clone,
            loaded_programs,
            fetch_context,
        )
        .await?;

        release_subs(&self.remote_account_provider, releases).await;

        Ok(FetchAndCloneBatchResult {
            result: FetchAndCloneResult {
                not_found_on_chain: not_found,
                missing_delegation_record,
            },
            materialized,
        })
    }
}

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    /// Determines whether an account finished undelegating on chain.
    pub(super) async fn should_refresh_undelegating_in_bank_account(
        &self,
        pubkey: &Pubkey,
        in_bank_slot: u64,
        delegated: bool,
        eata_pubkey: Option<Pubkey>,
        fetch_context: AccountFetchContext,
    ) -> RefreshDecision {
        {
            debug!(
                pubkey = %pubkey,
                delegated,
                undelegating = true,
                "Fetching undelegating account"
            );

            if let Some(eata_pubkey) = eata_pubkey {
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
                in_bank_slot,
                deleg_record,
                &self.validator_pubkey,
            ) {
                debug!(
                    "Account {pubkey} marked as undelegating will be overridden since undelegation completed"
                );
                self.invalidate_mirrored_record(pubkey);
                return RefreshDecision::Yes;
            }
        }
        RefreshDecision::No
    }

    /// Pairs a held account with its delegation record at a consistent slot.
    pub(super) fn task_to_fetch_with_delegation_record(
        &self,
        pubkey: Pubkey,
        account: AccountBuilder,
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
            use metrics::RecordMirrorLookupOutcome as MirrorOutcome;
            if record_slot > account_slot {
                metrics::inc_record_mirror_lookup(MirrorOutcome::Stale);
            } else if !is_delegation_record_data(&data) {
                metrics::inc_record_mirror_lookup(MirrorOutcome::ParseFallback);
            } else {
                metrics::inc_record_mirror_lookup(MirrorOutcome::Hit);
                let companion_account = AccountBuilder::from(Account {
                    lamports: MIRRORED_RECORD_LAMPORTS,
                    data,
                    owner: dlp_api::id(),
                    executable: false,
                    rent_epoch: 0,
                });
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
        let engine = self.engine.clone();
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
                        &engine,
                        pubkey,
                        companion_pubkey,
                        acc,
                        deleg,
                    )
                })
        })
    }

    pub(super) fn resolve_account_with_companion(
        engine: &Engine,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        acc: RemoteAccount,
        companion: RemoteAccount,
    ) -> ChainlinkResult<AccountWithCompanion> {
        use RemoteAccount::*;
        let accessor = engine.accounts();
        let loader = accessor.loader();
        let resolve = |account: &ResolvedAccount| match account {
            ResolvedAccount::Fresh(account) => Some(AccountBuilder::from(
                AccountSharedData::from(account.owned()),
            )),
            ResolvedAccount::Bank((pubkey, _)) => loader
                .read(pubkey, |account| {
                    AccountBuilder::from(AccountSharedData::from(
                        account.owned(),
                    ))
                })
                .ok()
                .flatten(),
        };
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
                match resolve(&acc.account) {
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
                let Some(comp_account) = resolve(&comp.account) else {
                    return Err(
                        ChainlinkError::ResolvedCompanionAccountCouldNoLongerBeFound(
                            companion_pubkey,
                        ),
                    );
                };
                let Some(account) = resolve(&acc.account) else {
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
}
