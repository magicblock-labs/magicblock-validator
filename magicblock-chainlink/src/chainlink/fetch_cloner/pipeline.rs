use std::{collections::HashSet, sync::atomic::Ordering};

use borsh::BorshDeserialize;
use compressed_delegation_client::CompressedDelegationRecord;
use dlp::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use magicblock_core::{token_programs::is_ata, traits::AccountsBank};
use magicblock_metrics::metrics::AccountFetchOrigin;
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;
use tracing::*;

use super::{
    subscription::{cancel_subs, CancelStrategy},
    types::{
        AccountWithCompanion, ClassifiedAccounts, ExistingSubs,
        PartitionedNotFound, ResolvedDelegatedAccounts, ResolvedPrograms,
    },
    FetchCloner,
};
use crate::{
    chainlink::errors::{ChainlinkError, ChainlinkResult},
    cloner::{errors::ClonerResult, AccountCloneRequest, Cloner},
    remote_account_provider::{
        photon_client::PhotonClient,
        program_account::{
            get_loaderv3_get_program_data_address, ProgramAccountResolver,
            LOADER_V3,
        },
        ChainPubsubClient, ChainRpcClient, MatchSlotsConfig, RemoteAccount,
        ResolvedAccount,
    },
};

pub(crate) fn build_existing_subs<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    pubkeys: &[Pubkey],
) -> ExistingSubs
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    let delegation_records = pubkeys
        .iter()
        .map(delegation_record_pda_from_delegated_account)
        .collect::<HashSet<_>>();
    let program_data_accounts = pubkeys
        .iter()
        .map(get_loaderv3_get_program_data_address)
        .collect::<HashSet<_>>();
    let existing_subs: HashSet<Pubkey> = pubkeys
        .iter()
        .chain(delegation_records.iter())
        .chain(program_data_accounts.iter())
        .filter(|x| this.is_watching(x))
        .copied()
        .collect();

    ExistingSubs { existing_subs }
}

/// Helper struct to hold classification buckets
#[derive(Default)]
struct ClassificationBuckets {
    not_found: Vec<(Pubkey, u64)>,
    plain: Vec<AccountCloneRequest>,
    owned_by_deleg: Vec<(Pubkey, AccountSharedData, u64)>,
    owned_by_deleg_compressed: Vec<(Pubkey, AccountSharedData, u64)>,
    programs: Vec<(Pubkey, AccountSharedData, u64)>,
    atas: Vec<(
        Pubkey,
        AccountSharedData,
        magicblock_core::token_programs::AtaInfo,
        u64,
    )>,
}

/// Classifies fetched remote accounts into categories
pub(crate) fn classify_remote_accounts(
    accs: Vec<RemoteAccount>,
    pubkeys: &[Pubkey],
) -> ClassifiedAccounts {
    let mut buckets = ClassificationBuckets::default();

    for (acc, &pubkey) in accs.into_iter().zip(pubkeys) {
        classify_single_account(acc, pubkey, &mut buckets);
    }

    ClassifiedAccounts {
        not_found: buckets.not_found,
        plain: buckets.plain,
        owned_by_deleg: buckets.owned_by_deleg,
        owned_by_deleg_compressed: buckets.owned_by_deleg_compressed,
        programs: buckets.programs,
        atas: buckets.atas,
    }
}

/// Helper function to classify a single remote account
#[inline]
fn classify_single_account(
    acc: RemoteAccount,
    pubkey: Pubkey,
    buckets: &mut ClassificationBuckets,
) {
    use RemoteAccount::*;
    match acc {
        NotFound(slot) => {
            buckets.not_found.push((pubkey, slot));
        }
        Found(remote_account_state) => {
            match remote_account_state.account {
                ResolvedAccount::Fresh(account_shared_data) => {
                    let slot = account_shared_data.remote_slot();

                    if account_shared_data.owner().eq(&dlp::id()) {
                        // Account owned by delegation program
                        buckets.owned_by_deleg.push((
                            pubkey,
                            account_shared_data,
                            slot,
                        ));
                    } else if account_shared_data
                        .owner()
                        .eq(&compressed_delegation_client::id())
                    {
                        buckets.owned_by_deleg_compressed.push((
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
                            &mut buckets.programs,
                        );
                    } else if let Some(ata) =
                        is_ata(&pubkey, &account_shared_data)
                    {
                        // Associated Token Account
                        buckets.atas.push((
                            pubkey,
                            account_shared_data,
                            ata,
                            slot,
                        ));
                    } else {
                        // Plain account
                        buckets.plain.push(AccountCloneRequest {
                            pubkey,
                            account: account_shared_data,
                            commit_frequency_ms: None,
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
#[instrument(skip(this, owned_by_deleg, plain, pubkeys, existing_subs), fields(pubkey_count = pubkeys.len()))]
pub(crate) async fn resolve_delegated_accounts<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    owned_by_deleg: Vec<(Pubkey, AccountSharedData, u64)>,
    plain: Vec<AccountCloneRequest>,
    min_context_slot: Option<u64>,
    fetch_origin: AccountFetchOrigin,
    pubkeys: &[Pubkey],
    existing_subs: HashSet<Pubkey>,
) -> ChainlinkResult<ResolvedDelegatedAccounts>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
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

    // We remove all new subs for accounts that were not found or already in the bank
    let (accounts_to_clone, record_subs) = {
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
            // Cancel all new subs since we won't clone any accounts
            cancel_subs(
                &this.remote_account_provider,
                CancelStrategy::New {
                    new_subs: pubkeys.iter().cloned().collect(),
                    existing_subs,
                },
            )
            .await;
            return Err(ChainlinkError::DelegatedAccountResolutionsFailed(
                errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }

        // Cancel new delegation record subs
        let mut record_subs = Vec::with_capacity(accounts_fully_resolved.len());
        let mut accounts_to_clone = plain;

        // Now process the accounts (this can fail without affecting unsubscription)
        for AccountWithCompanion {
            pubkey,
            mut account,
            companion_pubkey: delegation_record_pubkey,
            companion_account: delegation_record,
        } in accounts_fully_resolved.into_iter()
        {
            record_subs.push(delegation_record_pubkey);

            // If the account is delegated we set the owner and delegation state
            let (commit_frequency_ms, delegated_to_other) = if let Some(
                delegation_record_data,
            ) =
                delegation_record
            {
                // NOTE: failing here is fine when resolving all accounts for a transaction
                // since if something is off we better not run it anyways
                // However we may consider a different behavior when user is getting
                // multiple accounts.
                let delegation_record =
                    match FetchCloner::<T, U, V, C>::parse_delegation_record(
                        delegation_record_data.data(),
                        delegation_record_pubkey,
                    ) {
                        Ok(x) => x,
                        Err(err) => {
                            // Cancel all new subs since we won't clone any accounts
                            cancel_subs(
                                &this.remote_account_provider,
                                CancelStrategy::New {
                                    new_subs: pubkeys
                                        .iter()
                                        .cloned()
                                        .chain(record_subs.iter().cloned())
                                        .collect(),
                                    existing_subs: existing_subs.clone(),
                                },
                            )
                            .await;
                            return Err(err);
                        }
                    };

                trace!(pubkey = %pubkey, "Delegation record found");

                let delegated_to_other =
                    this.get_delegated_to_other(&delegation_record);

                let commit_freq = this.apply_delegation_record_to_account(
                    &mut account,
                    &delegation_record,
                );
                (commit_freq, delegated_to_other)
            } else {
                missing_delegation_record.push((pubkey, account.remote_slot()));
                (None, None)
            };
            accounts_to_clone.push(AccountCloneRequest {
                pubkey,
                account: account.into_account_shared_data(),
                commit_frequency_ms,
                delegated_to_other,
            });
        }

        (accounts_to_clone, record_subs)
    };

    Ok(ResolvedDelegatedAccounts {
        accounts_to_clone,
        record_subs,
        missing_delegation_record,
    })
}

pub(crate) async fn resolve_compressed_delegated_accounts<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    owned_by_deleg_compressed: Vec<(Pubkey, AccountSharedData, u64)>,
) -> ChainlinkResult<Vec<AccountCloneRequest>>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    owned_by_deleg_compressed
       .into_iter()
       .map(|(pubkey, mut account, _)| {
           let delegation_record =
               CompressedDelegationRecord::try_from_slice(account.data())
                   .map_err(|err| {
                       error!(
                           "Failed to deserialize compressed delegation record for {pubkey}: {err}\nAccount: {:?}",  
                           account
                       );
                       ChainlinkError::DelegatedAccountResolutionsFailed(
                           format!(
                               "Failed to deserialize compressed delegation record for {pubkey}: {err}"  
                           ),
                       )
                    })?;

            account.set_compressed(true);
            account.set_lamports(delegation_record.lamports);
            account.set_owner(delegation_record.owner);
            account.set_data(delegation_record.data);
            account.set_delegated(
                delegation_record
                .authority
                .eq(&this.validator_pubkey),
            );
            account.set_confined(delegation_record.authority.eq(&Pubkey::default()));

            let delegated_to_other =
            this.get_delegated_to_other(&DelegationRecord {
                authority: delegation_record.authority,
                owner: delegation_record.owner,
                delegation_slot: delegation_record.delegation_slot,
                lamports: delegation_record.lamports,
                commit_frequency_ms: 0,
            });
            Ok(AccountCloneRequest {
                pubkey,
                account,
                commit_frequency_ms: None,
                delegated_to_other,
            })
        })
    .collect::<ChainlinkResult<Vec<_>>>()
}

/// Resolves program accounts, fetching program data accounts for LoaderV3 programs
#[instrument(skip(this, programs, pubkeys, existing_subs), fields(pubkey_count = pubkeys.len()))]
pub(crate) async fn resolve_programs_with_program_data<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    programs: Vec<(Pubkey, AccountSharedData, u64)>,
    min_context_slot: Option<u64>,
    fetch_origin: AccountFetchOrigin,
    pubkeys: &[Pubkey],
    existing_subs: HashSet<Pubkey>,
) -> ChainlinkResult<ResolvedPrograms>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
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
                    let result = FetchCloner::<T, U, V, C, P>::resolve_account_with_companion(
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

    // Cancel subs for program data accounts
    let program_data_subs = accounts_with_program_data
        .iter()
        .map(|a| a.companion_pubkey)
        .collect::<HashSet<_>>();

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
        // Cancel all new subs since we won't clone any accounts
        cancel_subs(
            &this.remote_account_provider,
            CancelStrategy::New {
                new_subs: pubkeys
                    .iter()
                    .cloned()
                    .chain(program_data_subs.iter().cloned())
                    .collect(),
                existing_subs: existing_subs.clone(),
            },
        )
        .await;
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

/// Computes the subscription cancellation strategy based on what accounts were resolved
#[allow(unused_variables)] // Parameters used in cfg(test) block
pub(crate) fn compute_cancel_strategy(
    pubkeys: &[Pubkey],
    accounts_to_clone: &[AccountCloneRequest],
    loaded_programs: &[crate::remote_account_provider::program_account::LoadedProgram],
    record_subs: Vec<Pubkey>,
    program_data_subs: HashSet<Pubkey>,
    existing_subs: HashSet<Pubkey>,
    new_subs: HashSet<Pubkey>,
) -> CancelStrategy {
    // Cancel subs for delegated accounts (accounts we clone but don't need to watch)
    let delegated_accounts_to_cancel: HashSet<Pubkey> = accounts_to_clone
        .iter()
        .filter_map(|request| {
            if request.account.delegated() {
                Some(request.pubkey)
            } else {
                None
            }
        })
        .collect();

    // New approach: compute which subscriptions from new_subs should be cancelled
    // We want to cancel all new subscriptions except for:
    // - Accounts we cloned (both delegated and non-delegated are kept in new_subs)
    // - Programs we loaded
    // Note: Delegated accounts are cancelled separately via the 'all' field
    let accounts_to_keep: HashSet<Pubkey> = accounts_to_clone
        .iter()
        .map(|request| request.pubkey)
        .chain(loaded_programs.iter().map(|p| p.program_id))
        .collect();

    let new_subs_to_cancel: HashSet<Pubkey> =
        new_subs.difference(&accounts_to_keep).copied().collect();

    // Safety check: under test, verify new approach matches old approach
    #[cfg(test)]
    {
        // Old approach for comparison
        let accounts_not_cloned = pubkeys.iter().filter(|pubkey| {
            !accounts_to_clone
                .iter()
                .any(|request| request.pubkey.eq(pubkey))
                && !loaded_programs.iter().any(|p| p.program_id.eq(pubkey))
        });
        let old_new_subs_to_cancel: HashSet<Pubkey> = record_subs
            .iter()
            .cloned()
            .chain(accounts_not_cloned.into_iter().cloned().collect::<Vec<_>>())
            .chain(program_data_subs.iter().cloned())
            .collect();

        assert_eq!(
            new_subs_to_cancel, old_new_subs_to_cancel,
            "New subscription cancellation logic produces different result than old logic"
        );
    }

    CancelStrategy::Hybrid {
        new_subs: new_subs_to_cancel,
        existing_subs,
        all: delegated_accounts_to_cancel,
    }
}

/// Clones accounts and programs into the bank
#[instrument(skip(this, accounts_to_clone, loaded_programs))]
pub(crate) async fn clone_accounts_and_programs<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
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
    P: PhotonClient,
{
    let mut join_set = JoinSet::new();
    for request in accounts_to_clone {
        if tracing::enabled!(tracing::Level::TRACE) {
            trace!(
                pubkey = %request.pubkey,
                slot = request.account.remote_slot(),
                owner = %request.account.owner(),
                "Cloning account"
            );
        };

        let cloner = this.cloner.clone();
        join_set.spawn(async move { cloner.clone_account(request).await });
    }

    for acc in loaded_programs {
        if !this.is_program_allowed(&acc.program_id) {
            debug!(program_id = %acc.program_id, "Skipping clone of program");
            continue;
        }
        let cloner = this.cloner.clone();
        join_set.spawn(async move { cloner.clone_program(acc).await });
    }

    join_set
        .join_all()
        .await
        .into_iter()
        .collect::<ClonerResult<Vec<_>>>()?;

    Ok(())
}
