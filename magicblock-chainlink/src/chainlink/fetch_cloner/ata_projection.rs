use std::collections::HashSet;

use dlp_api::state::DelegationRecord;
use futures_util::future::join_all;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::{
    is_ata, try_derive_eata_address_and_bump, try_derive_supported_ata_pubkeys,
    AtaInfo, EphemeralAta, MaybeIntoAta, EATA_PROGRAM_ID,
};
use magicblock_metrics::metrics;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;
use tracing::*;

use super::{
    delegation,
    subscription::{acquire_subs, release_subs, SubscriptionRelease},
    types::AccountWithCompanion,
    FetchCloner,
};
use crate::{
    cloner::{AccountCloneRequest, Cloner, DelegationActions},
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, MatchSlotsConfig, RemoteAccount,
        ResolvedAccountSharedData, SubscriptionReason,
    },
};

pub(crate) fn derive_eata_pubkey_from_ata_account(
    ata_pubkey: &Pubkey,
    ata_account: &AccountSharedData,
) -> Option<Pubkey> {
    derive_eata_pubkey(is_ata(ata_pubkey, ata_account)?)
}

pub(crate) fn derive_eata_pubkey_from_ata_layout(
    ata_pubkey: &Pubkey,
    ata_account: &AccountSharedData,
) -> Option<Pubkey> {
    derive_eata_pubkey(ata_info_from_layout(ata_pubkey, ata_account)?)
}

fn derive_eata_pubkey(ata_info: AtaInfo) -> Option<Pubkey> {
    let (eata_pubkey, _) =
        try_derive_eata_address_and_bump(&ata_info.owner, &ata_info.mint)?;
    Some(eata_pubkey)
}

fn ata_info_from_layout(
    ata_pubkey: &Pubkey,
    ata_account: &AccountSharedData,
) -> Option<AtaInfo> {
    let data = ata_account.data();
    if data.len() < 64 {
        return None;
    }

    let mint = Pubkey::new_from_array(data[0..32].try_into().ok()?);
    let wallet_owner = Pubkey::new_from_array(data[32..64].try_into().ok()?);
    let ata_pubkeys = try_derive_supported_ata_pubkeys(&wallet_owner, &mint);
    if ata_pubkeys.contains(ata_pubkey) {
        return Some(AtaInfo {
            mint,
            owner: wallet_owner,
        });
    }

    None
}

pub(crate) fn is_known_empty_eata<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    eata_pubkey: &Pubkey,
) -> bool
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    this.known_empty_eatas.lock().get(eata_pubkey).is_some()
}

pub(crate) fn mark_eata_empty<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    eata_pubkey: Pubkey,
) where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    this.known_empty_eatas.lock().put(eata_pubkey, ());
}

pub(crate) fn maybe_build_projected_ata_clone_request_from_eata_sub_update<
    T,
    U,
    V,
    C,
>(
    this: &FetchCloner<T, U, V, C>,
    eata_pubkey: Pubkey,
    eata_account: &AccountSharedData,
    deleg_record: Option<&DelegationRecord>,
    delegation_actions: &DelegationActions,
) -> Option<AccountCloneRequest>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let deleg_record = deleg_record?;

    if deleg_record.authority != this.validator_pubkey {
        return None;
    }
    let (wallet_owner, mint) = delegation::parse_raw_eata_pda(
        &eata_pubkey,
        eata_account.data(),
        deleg_record.owner,
    )?;
    let ata_pubkeys = try_derive_supported_ata_pubkeys(&wallet_owner, &mint);

    // eATA updates only carry the projected balance fields. The in-bank ATA is
    // required as the base so the clone preserves the actual token program
    // owner and any Token-2022 account layout extensions.
    let mut ata_pubkey = None;
    let mut in_bank_ata = None;
    for candidate_pubkey in ata_pubkeys.token_2022_first().into_iter().flatten()
    {
        if let Some(candidate_account) =
            this.accounts_bank.get_account(&candidate_pubkey)
        {
            ata_pubkey = Some(candidate_pubkey);
            in_bank_ata = Some(candidate_account);
            break;
        }
    }
    let in_bank_ata = in_bank_ata.as_ref()?;
    let ata_pubkey = ata_pubkey?;
    if in_bank_ata.delegated() || in_bank_ata.undelegating() {
        return None;
    }
    let projected_ata = maybe_project_delegated_ata_from_eata(
        this,
        in_bank_ata,
        eata_account,
        deleg_record,
    )?;
    Some(AccountCloneRequest {
        pubkey: ata_pubkey,
        account: projected_ata,
        commit_frequency_ms: None,
        delegation_actions: delegation_actions.clone(),
        delegated_to_other: None,
    })
}

pub(crate) async fn maybe_project_ata_from_subscription_update<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    ata_pubkey: Pubkey,
    ata_account: AccountSharedData,
) -> (
    AccountSharedData,
    Option<(DelegationRecord, Option<DelegationActions>)>,
)
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let Some(ata_info) = is_ata(&ata_pubkey, &ata_account) else {
        return (ata_account, None);
    };

    let Some((eata_pubkey, _)) =
        try_derive_eata_address_and_bump(&ata_info.owner, &ata_info.mint)
    else {
        return (ata_account, None);
    };

    let was_watching = this.remote_account_provider.is_watching(&eata_pubkey);

    // Ensure before cache checks; this keeps the subscription LRU warm
    // without refcounting the projection reason on every ATA update.
    let subscribed = match this
        .ensure_subscription(&eata_pubkey, SubscriptionReason::AtaProjection)
        .await
    {
        Ok(()) => true,
        Err(err) => {
            warn!(
                pubkey = %eata_pubkey,
                error = ?err,
                "Failed to subscribe to derived eATA"
            );
            false
        }
    };

    // Known-empty eATAs skip the fetch only if the subscription was already live.
    if was_watching && subscribed && is_known_empty_eata(this, &eata_pubkey) {
        return (ata_account, None);
    }

    let (eata_account, definitively_not_found) = match this
        .remote_account_provider
        .try_get_multi_until_slots_match(
            &[eata_pubkey],
            Some(MatchSlotsConfig {
                min_context_slot: Some(ata_account.remote_slot()),
                ..Default::default()
            }),
            metrics::AccountFetchOrigin::ProjectAta,
        )
        .await
    {
        Ok(mut accounts) => {
            let popped = accounts.pop();
            // Only `NotFound` proves absence; stale, missing, or failed fetches retry later.
            let nf = matches!(popped, Some(RemoteAccount::NotFound(_)));
            let fresh = popped.and_then(|a| a.fresh_account());
            (fresh, nf)
        }
        Err(err) => {
            debug!(
                pubkey = %eata_pubkey,
                error = ?err,
                "Failed to fetch eATA for projection"
            );
            (None, false)
        }
    };

    let Some(eata_account) = eata_account else {
        // Cache absence only after a confirmed NotFound and live subscription.
        if definitively_not_found && subscribed {
            mark_eata_empty(this, eata_pubkey);
        }
        return (ata_account, None);
    };

    let deleg_record = delegation::fetch_and_parse_delegation_record(
        this,
        eata_pubkey,
        ata_account.remote_slot().max(eata_account.remote_slot()),
        metrics::AccountFetchOrigin::ProjectAta,
    )
    .await;

    let Some(deleg_record) = deleg_record else {
        return (ata_account, None);
    };
    let (deleg_record, delegation_actions) = deleg_record;

    if let Some(projected_ata) = maybe_project_delegated_ata_from_eata(
        this,
        &ata_account,
        &eata_account,
        &deleg_record,
    ) {
        return (projected_ata, Some((deleg_record, delegation_actions)));
    }
    (ata_account, Some((deleg_record, delegation_actions)))
}

pub(crate) fn maybe_project_delegated_ata_from_eata<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    ata_account: &AccountSharedData,
    eata_account: &AccountSharedData,
    deleg_record: &DelegationRecord,
) -> Option<AccountSharedData>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    if deleg_record.authority != this.validator_pubkey {
        return None;
    }

    // Projecting from eATA must preserve the base ATA's owner and data length.
    // That is what keeps Token-2022 accounts from being rebuilt as legacy SPL
    // Token accounts when the eATA itself only stores owner, mint, and amount.
    let projected_from_base_ata = if deleg_record.owner == EATA_PROGRAM_ID {
        EphemeralAta::try_from_account_data(eata_account.data())
            .and_then(|eata| eata.project_into_ata_account(ata_account))
    } else {
        None
    };

    let mut projected_ata = match projected_from_base_ata
        .or_else(|| eata_account.maybe_into_ata(deleg_record.owner))
    {
        Some(projected_ata) => projected_ata,
        None => {
            return None;
        }
    };
    let projected_slot =
        ata_account.remote_slot().max(eata_account.remote_slot());
    projected_ata.set_remote_slot(projected_slot);
    projected_ata.set_delegated(true);
    Some(projected_ata)
}

/// Resolves ATAs with eATA projection.
/// For each detected ATA, we derive the eATA PDA, subscribe to both,
/// and, if the ATA is delegated to us and the eATA exists, we clone the eATA data
/// into the ATA in the bank.
#[instrument(skip(this, atas))]
pub(crate) async fn resolve_ata_with_eata_projection<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    atas: Vec<(
        Pubkey,
        AccountSharedData,
        magicblock_core::token_programs::AtaInfo,
        u64,
    )>,
    min_context_slot: Option<u64>,
    fetch_origin: metrics::AccountFetchOrigin,
) -> Vec<AccountCloneRequest>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    if atas.is_empty() {
        return vec![];
    }

    let mut accounts_to_clone = vec![];
    let mut ata_join_set = JoinSet::new();

    // Collect all pubkeys to subscribe to and spawn fetch tasks
    let mut pubkeys_to_subscribe = vec![];

    for (ata_pubkey, _, ata_info, ata_account_slot) in &atas {
        // Collect ATA pubkey for subscription
        pubkeys_to_subscribe.push(*ata_pubkey);

        let effective_slot = if let Some(min_slot) = min_context_slot {
            min_slot.max(*ata_account_slot)
        } else {
            *ata_account_slot
        };

        if let Some((eata, _)) =
            try_derive_eata_address_and_bump(&ata_info.owner, &ata_info.mint)
        {
            // Collect eATA pubkey for subscription
            pubkeys_to_subscribe.push(eata);

            ata_join_set.spawn(FetchCloner::task_to_fetch_with_companion(
                this,
                *ata_pubkey,
                eata,
                effective_slot,
                fetch_origin,
            ));
        } else {
            // eATA derivation failed, but still queue the ATA for cloning
            // without a companion by using a dummy companion pubkey
            // The resolve_account_with_companion logic handles the case
            // where the companion is not found
            ata_join_set.spawn(FetchCloner::task_to_fetch_with_companion(
                this,
                *ata_pubkey,
                Pubkey::default(), // Dummy companion - will be marked as NotFound
                effective_slot,
                fetch_origin,
            ));
        }
    }

    // Deduplicate pubkeys to avoid redundant subscribe calls
    pubkeys_to_subscribe = pubkeys_to_subscribe
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let acquired_projection_subs = acquire_subs(
        &this.remote_account_provider,
        pubkeys_to_subscribe.clone(),
        SubscriptionReason::AtaProjection,
    )
    .await
    .map(|_| true)
    .unwrap_or_else(|err| {
        warn!(error = ?err, "Failed to subscribe to ATA/eATA account");
        false
    });

    let ata_results = ata_join_set.join_all().await;

    // Phase 1: Collect successfully resolved ATAs
    struct AtaResolutionInput {
        ata_pubkey: Pubkey,
        ata_account: ResolvedAccountSharedData,
        eata_pubkey: Pubkey,
        eata_shared: Option<AccountSharedData>,
    }

    let mut ata_inputs: Vec<AtaResolutionInput> = Vec::new();

    for result in ata_results {
        let AccountWithCompanion {
            pubkey: ata_pubkey,
            account: ata_account,
            companion_pubkey: eata_pubkey,
            companion_account: maybe_eata_account,
        } = match result {
            Ok(Ok(v)) => v,
            Ok(Err(err)) => {
                warn!(error = %err, "Failed to resolve ATA/eATA companion");
                continue;
            }
            Err(join_err) => {
                warn!(error = %join_err, "Failed to join ATA/eATA fetch task");
                continue;
            }
        };

        let eata_shared =
            maybe_eata_account.map(|e| e.account_shared_data_cloned());
        ata_inputs.push(AtaResolutionInput {
            ata_pubkey,
            ata_account,
            eata_pubkey,
            eata_shared,
        });
    }

    // Phase 2: Fetch delegation records in parallel for all eATAs
    let deleg_futures = ata_inputs.iter().filter_map(|input| {
        input.eata_shared.as_ref().map(|_| {
            delegation::fetch_and_parse_delegation_record(
                this,
                input.eata_pubkey,
                this.remote_account_provider.chain_slot(),
                fetch_origin,
            )
        })
    });
    let deleg_results: Vec<
        Option<(DelegationRecord, Option<DelegationActions>)>,
    > = join_all(deleg_futures).await;

    // Phase 3: Combine results
    let mut deleg_iter = deleg_results.into_iter();
    for input in ata_inputs {
        let mut account_to_clone =
            input.ata_account.account_shared_data_cloned();
        let mut commit_frequency_ms = None;
        let mut delegated_to_other = None;
        let mut actions = None;

        if let Some(eata_shared) = &input.eata_shared {
            if let Some(Some(deleg)) = deleg_iter.next() {
                let (deleg_record, delegation_actions) = deleg;
                delegated_to_other =
                    delegation::get_delegated_to_other(this, &deleg_record);
                commit_frequency_ms = Some(deleg_record.commit_frequency_ms);

                if let Some(projected_ata) =
                    maybe_project_delegated_ata_from_eata(
                        this,
                        input.ata_account.account_shared_data(),
                        eata_shared,
                        &deleg_record,
                    )
                {
                    account_to_clone = projected_ata;
                    actions = delegation_actions;
                }
            }
        }

        accounts_to_clone.push(AccountCloneRequest {
            pubkey: input.ata_pubkey,
            account: account_to_clone,
            commit_frequency_ms,
            delegation_actions: actions.unwrap_or_default(),
            delegated_to_other,
        });
    }

    let mut releases = pubkeys_to_subscribe
        .iter()
        .copied()
        .map(|pubkey| SubscriptionRelease::Pubkey {
            pubkey,
            reason: SubscriptionReason::DirectAccount,
        })
        .collect::<Vec<_>>();
    if acquired_projection_subs {
        releases.extend(pubkeys_to_subscribe.iter().copied().map(|pubkey| {
            SubscriptionRelease::Pubkey {
                pubkey,
                reason: SubscriptionReason::AtaProjection,
            }
        }));
    }
    release_subs(&this.remote_account_provider, releases).await;

    accounts_to_clone
}
