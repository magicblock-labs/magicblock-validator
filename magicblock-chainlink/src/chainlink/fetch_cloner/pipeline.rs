use std::{collections::HashSet, sync::atomic::Ordering};

use dlp_api::pda::delegation_record_pda_from_delegated_account;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::is_ata;
use magicblock_metrics::metrics::AccountFetchOrigin;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;
use tracing::*;

use super::{
    subscription::{acquire_subs, release_subs, SubscriptionRelease},
    types::{
        AccountWithCompanion, ClassifiedAccounts, PartitionedNotFound,
        ResolvedDelegatedAccounts, ResolvedPrograms,
    },
    FetchCloner,
};
use crate::{
    chainlink::errors::{ChainlinkError, ChainlinkResult},
    cloner::{
        errors::ClonerResult, AccountCloneRequest, Cloner, DelegationActions,
    },
    remote_account_provider::{
        program_account::{
            get_loaderv3_get_program_data_address, ProgramAccountResolver,
            LOADER_V3,
        },
        ChainPubsubClient, ChainRpcClient, MatchSlotsConfig, RemoteAccount,
        ResolvedAccount, SubscriptionReason,
    },
};

pub(crate) fn collect_delegation_action_dependencies(
    accounts_to_clone: &[AccountCloneRequest],
) -> HashSet<Pubkey> {
    let mut dependencies = HashSet::new();
    for request in accounts_to_clone {
        for instruction in request.delegation_actions.iter() {
            dependencies.insert(instruction.program_id);
            for account_meta in &instruction.accounts {
                dependencies.insert(account_meta.pubkey);
            }
        }
    }
    dependencies
}

/// Classifies fetched remote accounts into categories
pub(crate) fn classify_remote_accounts(
    accs: Vec<RemoteAccount>,
    pubkeys: &[Pubkey],
) -> ClassifiedAccounts {
    let mut not_found = Vec::new();
    let mut plain = Vec::new();
    let mut owned_by_deleg = Vec::new();
    let mut programs = Vec::new();
    let mut atas = Vec::new();

    for (acc, &pubkey) in accs.into_iter().zip(pubkeys) {
        classify_single_account(
            acc,
            pubkey,
            &mut not_found,
            &mut plain,
            &mut owned_by_deleg,
            &mut programs,
            &mut atas,
        );
    }

    ClassifiedAccounts {
        not_found,
        plain,
        owned_by_deleg,
        programs,
        atas,
    }
}

/// Helper function to classify a single remote account
#[inline]
fn classify_single_account(
    acc: RemoteAccount,
    pubkey: Pubkey,
    not_found: &mut Vec<(Pubkey, u64)>,
    plain: &mut Vec<AccountCloneRequest>,
    owned_by_deleg: &mut Vec<(Pubkey, AccountSharedData, u64)>,
    programs: &mut Vec<(Pubkey, AccountSharedData, u64)>,
    atas: &mut Vec<(
        Pubkey,
        AccountSharedData,
        magicblock_core::token_programs::AtaInfo,
        u64,
    )>,
) {
    use RemoteAccount::*;
    match acc {
        NotFound(slot) => {
            not_found.push((pubkey, slot));
        }
        Found(remote_account_state) => {
            match remote_account_state.account {
                ResolvedAccount::Fresh(account_shared_data) => {
                    let slot = account_shared_data.remote_slot();

                    if account_shared_data.owner().eq(&dlp_api::id()) {
                        // Account owned by delegation program
                        owned_by_deleg.push((
                            pubkey,
                            account_shared_data,
                            slot,
                        ));
                    } else if account_shared_data.executable() {
                        // Executable program account
                        classify_program(
                            pubkey,
                            account_shared_data,
                            slot,
                            programs,
                        );
                    } else if let Some(ata) =
                        is_ata(&pubkey, &account_shared_data)
                    {
                        // Associated Token Account
                        atas.push((pubkey, account_shared_data, ata, slot));
                    } else {
                        // Plain account
                        plain.push(AccountCloneRequest {
                            pubkey,
                            account: account_shared_data,
                            commit_frequency_ms: None,
                            delegation_actions: DelegationActions::default(),
                            delegated_to_other: None,
                        });
                    }
                }
                ResolvedAccount::Bank((pubkey, slot)) => {
                    error!(pubkey = %pubkey, slot = slot, "BUG: Should not be fetching accounts already in bank");
                }
            }
        }
    }
}

/// Classifies an executable program account, filtering out native loader programs
#[inline]
fn classify_program(
    pubkey: Pubkey,
    account_shared_data: AccountSharedData,
    slot: u64,
    programs: &mut Vec<(Pubkey, AccountSharedData, u64)>,
) {
    // We don't clone native loader programs.
    // They should not pass the blacklist in the first place,
    // but in case a new native program is introduced we don't want to fail
    if account_shared_data
        .owner()
        .eq(&solana_sdk_ids::native_loader::id())
    {
        warn!(
            "Not cloning native loader program account: {pubkey} (should have been blacklisted)",
        );
    } else {
        programs.push((pubkey, account_shared_data, slot));
    }
}

/// Partitions not_found accounts into those to clone as empty and those to leave as not found
pub(crate) fn partition_not_found(
    mark_empty_if_not_found: Option<&[Pubkey]>,
    not_found: Vec<(Pubkey, u64)>,
) -> PartitionedNotFound {
    if let Some(mark_empty) = mark_empty_if_not_found {
        let (clone_as_empty, not_found) = not_found
            .into_iter()
            .partition::<Vec<_>, _>(|(p, _)| mark_empty.contains(p));
        PartitionedNotFound {
            clone_as_empty,
            not_found,
        }
    } else {
        PartitionedNotFound {
            clone_as_empty: vec![],
            not_found,
        }
    }
}

/// Resolves delegated accounts by fetching their delegation records
#[instrument(skip(this, owned_by_deleg, plain), fields(pubkey_count = owned_by_deleg.len()))]
pub(crate) async fn resolve_delegated_accounts<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    owned_by_deleg: Vec<(Pubkey, AccountSharedData, u64)>,
    plain: Vec<AccountCloneRequest>,
    min_context_slot: Option<u64>,
    fetch_origin: AccountFetchOrigin,
) -> ChainlinkResult<ResolvedDelegatedAccounts>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let record_subs = owned_by_deleg
        .iter()
        .map(|(pubkey, _, _)| {
            delegation_record_pda_from_delegated_account(pubkey)
        })
        .collect::<Vec<_>>();
    acquire_subs(
        &this.remote_account_provider,
        record_subs.clone(),
        SubscriptionReason::DelegationRecord,
    )
    .await?;

    // For potentially delegated accounts we update the owner and delegation state first
    let mut fetch_with_delegation_record_join_set = JoinSet::new();
    for (pubkey, _, account_slot) in &owned_by_deleg {
        let effective_slot = if let Some(min_slot) = min_context_slot {
            min_slot.max(*account_slot)
        } else {
            *account_slot
        };
        fetch_with_delegation_record_join_set.spawn(
            this.task_to_fetch_with_delegation_record(
                *pubkey,
                effective_slot,
                fetch_origin,
            ),
        );
    }

    let mut missing_delegation_record = vec![];

    let accounts_to_clone = {
        let joined = fetch_with_delegation_record_join_set.join_all().await;
        let (errors, accounts_fully_resolved) = joined.into_iter().fold(
            (vec![], vec![]),
            |(mut errors, mut successes), res| {
                match res {
                    Ok(Ok(account_with_deleg)) => {
                        successes.push(account_with_deleg)
                    }
                    Ok(Err(err)) => errors.push(err),
                    Err(err) => errors.push(err.into()),
                }
                (errors, successes)
            },
        );

        // If we encounter any error while fetching delegated accounts then
        // we have to abort as we cannot resume without the ability to sync
        // with the remote
        if !errors.is_empty() {
            let releases = owned_by_deleg
                .iter()
                .map(|(pubkey, _, _)| *pubkey)
                .chain(owned_by_deleg.iter().map(|(pubkey, _, _)| *pubkey))
                .chain(record_subs.iter().copied())
                .map(|pubkey| SubscriptionRelease::Pubkey {
                    pubkey,
                    reason: SubscriptionReason::DirectAccount,
                })
                .chain(record_subs.iter().copied().map(|pubkey| {
                    SubscriptionRelease::Pubkey {
                        pubkey,
                        reason: SubscriptionReason::DelegationRecord,
                    }
                }))
                .collect::<Vec<_>>();
            release_subs(&this.remote_account_provider, releases).await;
            return Err(ChainlinkError::DelegatedAccountResolutionsFailed(
                errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }

        let mut accounts_to_clone = plain;

        // Collect unique owner programs to subscribe to concurrently
        let mut owner_programs_to_subscribe: HashSet<Pubkey> = HashSet::new();

        // Now process the accounts (this can fail without affecting unsubscription)
        for AccountWithCompanion {
            pubkey,
            mut account,
            companion_pubkey: delegation_record_pubkey,
            companion_account: delegation_record,
        } in accounts_fully_resolved.into_iter()
        {
            // If the account is delegated we set the owner and delegation state
            let (commit_frequency_ms, delegated_to_other, delegation_actions) =
                if let Some(delegation_record_data) = delegation_record {
                    // NOTE: failing here is fine when resolving all accounts for a transaction
                    // since if something is off we better not run it anyways
                    // However we may consider a different behavior when user is getting
                    // multiple accounts.
                    let (delegation_record, delegation_actions) = match this
                        .parse_delegation_record(
                            delegation_record_data.data(),
                            delegation_record_pubkey,
                        ) {
                        Ok(x) => x,
                        Err(err) => {
                            let releases = owned_by_deleg
                                .iter()
                                .map(|(pubkey, _, _)| *pubkey)
                                .chain(
                                    owned_by_deleg
                                        .iter()
                                        .map(|(pubkey, _, _)| *pubkey),
                                )
                                .chain(record_subs.iter().copied())
                                .map(|pubkey| SubscriptionRelease::Pubkey {
                                    pubkey,
                                    reason: SubscriptionReason::DirectAccount,
                                })
                                .chain(record_subs.iter().copied().map(|pubkey| {
                                    SubscriptionRelease::Pubkey {
                                        pubkey,
                                        reason: SubscriptionReason::DelegationRecord,
                                    }
                                }))
                                .collect::<Vec<_>>();
                            release_subs(
                                &this.remote_account_provider,
                                releases,
                            )
                            .await;
                            return Err(err);
                        }
                    };

                    trace!(pubkey = %pubkey, "Delegation record found");

                    let delegated_to_other =
                        this.get_delegated_to_other(&delegation_record);

                    let commit_freq = this.apply_delegation_record_to_account(
                        pubkey,
                        &mut account,
                        &delegation_record,
                    );

                    // Skip high-cardinality owner programs such as SPL Token.
                    if account.delegated()
                        && !this
                            .programs_not_to_subscribe
                            .contains(&delegation_record.owner)
                    {
                        owner_programs_to_subscribe
                            .insert(delegation_record.owner);
                    }

                    let delegation_actions = if account.delegated() {
                        delegation_actions.unwrap_or_default()
                    } else {
                        DelegationActions::default()
                    };

                    (commit_freq, delegated_to_other, delegation_actions)
                } else {
                    missing_delegation_record
                        .push((pubkey, account.remote_slot()));
                    (None, None, DelegationActions::default())
                };
            accounts_to_clone.push(AccountCloneRequest {
                pubkey,
                account: account.into_account_shared_data(),
                commit_frequency_ms,
                delegation_actions,
                delegated_to_other,
            });
        }

        release_subs(
            &this.remote_account_provider,
            owned_by_deleg
                .iter()
                .map(|(pubkey, _, _)| SubscriptionRelease::Pubkey {
                    pubkey: *pubkey,
                    reason: SubscriptionReason::DirectAccount,
                })
                .collect::<Vec<_>>(),
        )
        .await;

        // Subscribe to owner programs concurrently in background (best-effort)
        if !owner_programs_to_subscribe.is_empty() {
            let remote_account_provider = this.remote_account_provider.clone();
            tokio::spawn(async move {
                let subscribe_futures =
                    owner_programs_to_subscribe.into_iter().map(|owner| {
                        let provider = remote_account_provider.clone();
                        async move {
                            let result =
                                provider.subscribe_program(owner).await;
                            (owner, result)
                        }
                    });
                let results =
                    futures_util::future::join_all(subscribe_futures).await;
                for (owner, result) in results {
                    if let Err(err) = result {
                        warn!(program_id = %owner, error = %err, "Failed to subscribe to owner program");
                    }
                }
            });
        }

        accounts_to_clone
    };

    Ok(ResolvedDelegatedAccounts {
        accounts_to_clone,
        record_subs,
        missing_delegation_record,
    })
}

/// Resolves program accounts, fetching program data accounts for LoaderV3 programs
#[instrument(skip(this, programs), fields(pubkey_count = programs.len()))]
pub(crate) async fn resolve_programs_with_program_data<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    programs: Vec<(Pubkey, AccountSharedData, u64)>,
    min_context_slot: Option<u64>,
    fetch_origin: AccountFetchOrigin,
) -> ChainlinkResult<ResolvedPrograms>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    // For LoaderV3 accounts we fetch the program data account
    let (loaderv3_programs, single_account_programs): (Vec<_>, Vec<_>) =
        programs
            .into_iter()
            .partition(|(_, acc, _)| acc.owner().eq(&LOADER_V3));

    let mut pubkeys_to_fetch = Vec::with_capacity(loaderv3_programs.len() * 2);
    let mut batch_min_context_slot = min_context_slot;

    for (pubkey, _, account_slot) in &loaderv3_programs {
        let effective_slot = if let Some(min_slot) = min_context_slot {
            min_slot.max(*account_slot)
        } else {
            *account_slot
        };
        batch_min_context_slot =
            Some(batch_min_context_slot.unwrap_or(0).max(effective_slot));

        // We intentionally take the global max effective slot for the batch (not per-program)
        // to enforce a consistent minimum slot across all LoaderV3 programs.
        let program_data_pubkey = get_loaderv3_get_program_data_address(pubkey);
        pubkeys_to_fetch.push(*pubkey);
        pubkeys_to_fetch.push(program_data_pubkey);
    }

    let program_data_subs = loaderv3_programs
        .iter()
        .map(|(pubkey, _, _)| get_loaderv3_get_program_data_address(pubkey))
        .collect::<HashSet<_>>();
    acquire_subs(
        &this.remote_account_provider,
        program_data_subs.iter().copied().collect::<Vec<_>>(),
        SubscriptionReason::ProgramData,
    )
    .await?;

    let fetch_result = if !pubkeys_to_fetch.is_empty() {
        this.fetch_count
            .fetch_add(pubkeys_to_fetch.len() as u64, Ordering::Relaxed);
        this.remote_account_provider
            .try_get_multi_until_slots_match(
                &pubkeys_to_fetch,
                Some(MatchSlotsConfig {
                    min_context_slot: batch_min_context_slot,
                    ..Default::default()
                }),
                fetch_origin,
            )
            .await
    } else {
        Ok(vec![])
    };

    let (mut errors, accounts_with_program_data) = match fetch_result {
        Ok(remote_accounts) => {
            if remote_accounts.len() != pubkeys_to_fetch.len() {
                (
                    vec![ChainlinkError::ProgramAccountResolutionsFailed(
                        format!(
                            "LoaderV3 fetch: expected {} accounts, got {}",
                            pubkeys_to_fetch.len(),
                            remote_accounts.len()
                        ),
                    )],
                    vec![],
                )
            } else {
                let mut successes = Vec::new();
                let mut errors = Vec::new();

                for (program_info, (pubkey_pair, account_pair)) in
                    loaderv3_programs.into_iter().zip(
                        pubkeys_to_fetch
                            .chunks(2)
                            .zip(remote_accounts.chunks(2)),
                    )
                {
                    if account_pair.len() != 2 {
                        errors.push(ChainlinkError::ProgramAccountResolutionsFailed(
                            format!("LoaderV3 fetch: expected 2 accounts (program + data) per pair, got {}", account_pair.len())
                        ));
                        continue;
                    }
                    let (pubkey, _, _) = program_info;
                    let program_data_pubkey = pubkey_pair[1];

                    let account_program = account_pair[0].clone();
                    let account_data = account_pair[1].clone();
                    let result = FetchCloner::<T, U, V, C>::resolve_account_with_companion(
                        &this.accounts_bank,
                        pubkey,
                        program_data_pubkey,
                        account_program,
                        account_data,
                    );
                    match result {
                        Ok(res) => successes.push(res),
                        Err(err) => errors.push(err),
                    }
                }
                (errors, successes)
            }
        }
        Err(err) => (vec![ChainlinkError::from(err)], vec![]),
    };

    let mut loaded_programs = vec![];

    for AccountWithCompanion {
        pubkey: program_id,
        account: program_account,
        companion_pubkey: program_data_pubkey,
        companion_account: program_data,
    } in accounts_with_program_data.into_iter()
    {
        if let Some(program_data) = program_data {
            let owner = *program_account.owner();
            let program_data_account = program_data.into_account_shared_data();
            let loaded_program = ProgramAccountResolver::try_new(
                program_id,
                owner,
                None,
                Some(program_data_account),
            )?
            .into_loaded_program();
            loaded_programs.push(loaded_program);
        } else {
            errors.push(ChainlinkError::FailedToResolveProgramDataAccount(
                program_data_pubkey,
                program_id,
            ));
        }
    }
    for (program_id, program_account, _) in single_account_programs {
        let owner = *program_account.owner();
        let loaded_program = ProgramAccountResolver::try_new(
            program_id,
            owner,
            Some(program_account),
            None,
        )?
        .into_loaded_program();
        loaded_programs.push(loaded_program);
    }

    if !errors.is_empty() {
        let releases = pubkeys_to_fetch
            .iter()
            .copied()
            .map(|pubkey| SubscriptionRelease::Pubkey {
                pubkey,
                reason: SubscriptionReason::DirectAccount,
            })
            .chain(program_data_subs.iter().copied().map(|pubkey| {
                SubscriptionRelease::Pubkey {
                    pubkey,
                    reason: SubscriptionReason::ProgramData,
                }
            }))
            .collect::<Vec<_>>();
        release_subs(&this.remote_account_provider, releases).await;
        return Err(ChainlinkError::ProgramAccountResolutionsFailed(
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }

    Ok(ResolvedPrograms {
        loaded_programs,
        program_data_subs,
    })
}

pub(crate) fn compute_subscription_releases(
    all_requested_pubkeys: &[Pubkey],
    accounts_to_clone: &[AccountCloneRequest],
    loaded_programs: &[crate::remote_account_provider::program_account::LoadedProgram],
    record_subs: Vec<Pubkey>,
    program_data_subs: HashSet<Pubkey>,
) -> Vec<SubscriptionRelease> {
    let cloned_accounts = accounts_to_clone
        .iter()
        .map(|request| request.pubkey)
        .collect::<HashSet<_>>();
    let loaded_program_ids = loaded_programs
        .iter()
        .map(|program| program.program_id)
        .collect::<HashSet<_>>();
    let delegated_cloned_accounts = accounts_to_clone
        .iter()
        .filter(|request| request.account.delegated())
        .map(|request| request.pubkey)
        .collect::<HashSet<_>>();

    let mut direct_releases = all_requested_pubkeys
        .iter()
        .copied()
        .filter(|pubkey| {
            !cloned_accounts.contains(pubkey)
                && !loaded_program_ids.contains(pubkey)
        })
        .collect::<HashSet<_>>();
    direct_releases.extend(delegated_cloned_accounts);

    let mut releases = direct_releases
        .into_iter()
        .map(|pubkey| SubscriptionRelease::Pubkey {
            pubkey,
            reason: SubscriptionReason::DirectAccount,
        })
        .collect::<Vec<_>>();
    releases.extend(record_subs.into_iter().map(|pubkey| {
        SubscriptionRelease::Pubkey {
            pubkey,
            reason: SubscriptionReason::DelegationRecord,
        }
    }));
    releases.extend(program_data_subs.into_iter().map(|pubkey| {
        SubscriptionRelease::Pubkey {
            pubkey,
            reason: SubscriptionReason::ProgramData,
        }
    }));
    releases
}

/// Clones accounts and programs into the bank
#[instrument(skip(this, accounts_to_clone, loaded_programs))]
pub(crate) async fn clone_accounts_and_programs<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    accounts_to_clone: Vec<AccountCloneRequest>,
    loaded_programs: Vec<
        crate::remote_account_provider::program_account::LoadedProgram,
    >,
) -> ClonerResult<()>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    // 1) Clone programs first so post-delegation action instructions can load
    // their program accounts when action txs are sent during account cloning.
    let mut program_join_set = JoinSet::new();
    for acc in loaded_programs {
        if !this.is_program_allowed(&acc.program_id) {
            debug!(program_id = %acc.program_id, "Skipping clone of program");
            continue;
        }
        let cloner = this.cloner.clone();
        program_join_set.spawn(async move { cloner.clone_program(acc).await });
    }
    program_join_set
        .join_all()
        .await
        .into_iter()
        .collect::<ClonerResult<Vec<_>>>()?;

    // 2) Clone accounts without post-delegation actions first so all action
    // dependencies are materialized in the bank before action tx execution.
    let (accounts_with_actions, accounts_without_actions): (Vec<_>, Vec<_>) =
        accounts_to_clone
            .into_iter()
            .partition(|request| !request.delegation_actions.is_empty());

    let mut accounts_join_set = JoinSet::new();
    for request in accounts_without_actions {
        if tracing::enabled!(tracing::Level::TRACE) {
            trace!(
                pubkey = %request.pubkey,
                slot = request.account.remote_slot(),
                owner = %request.account.owner(),
                "Cloning account"
            );
        };

        let this_clone = this.clone();
        accounts_join_set.spawn(async move {
            this_clone.clone_account_with_ownership(request).await
        });
    }
    accounts_join_set
        .join_all()
        .await
        .into_iter()
        .collect::<ClonerResult<Vec<_>>>()?;

    // 3) Finally clone accounts that carry post-delegation actions.
    let mut action_accounts_join_set = JoinSet::new();
    for request in accounts_with_actions {
        if tracing::enabled!(tracing::Level::TRACE) {
            trace!(
                pubkey = %request.pubkey,
                slot = request.account.remote_slot(),
                owner = %request.account.owner(),
                "Cloning account with delegation actions"
            );
        };

        let this_clone = this.clone();
        action_accounts_join_set.spawn(async move {
            this_clone.clone_account_with_ownership(request).await
        });
    }
    action_accounts_join_set
        .join_all()
        .await
        .into_iter()
        .collect::<ClonerResult<Vec<_>>>()?;

    Ok(())
}
