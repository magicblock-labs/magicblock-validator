use dlp::state::DelegationRecord;
use futures_util::future::join_all;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::try_derive_eata_address_and_bump;
use magicblock_metrics::metrics;
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;
use std::collections::HashSet;
use tokio::task::JoinSet;
use tracing::*;

use super::{delegation, types::AccountWithCompanion, FetchCloner};
use crate::{
    cloner::{AccountCloneRequest, Cloner},
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, ResolvedAccountSharedData,
    },
};

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

    // Subscribe to all ATA and eATA accounts in parallel
    let subscription_results = join_all(
        pubkeys_to_subscribe
            .iter()
            .map(|pk| this.subscribe_to_account(pk)),
    )
    .await;

    for (pubkey, result) in
        pubkeys_to_subscribe.iter().zip(subscription_results)
    {
        if let Err(err) = result {
            warn!(
                pubkey = %pubkey,
                err = ?err,
                "Failed to subscribe to ATA/eATA account"
            );
        }
    }

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
    let deleg_results: Vec<Option<DelegationRecord>> =
        join_all(deleg_futures).await;

    // Phase 3: Combine results
    let mut deleg_iter = deleg_results.into_iter();
    for input in ata_inputs {
        let mut account_to_clone =
            input.ata_account.account_shared_data_cloned();
        let mut commit_frequency_ms = None;
        let mut delegated_to_other = None;

        if let Some(eata_shared) = &input.eata_shared {
            if let Some(Some(deleg)) = deleg_iter.next() {
                delegated_to_other =
                    delegation::get_delegated_to_other(this, &deleg);
                commit_frequency_ms = Some(deleg.commit_frequency_ms);

                if let Some(projected_ata) = this
                    .maybe_project_delegated_ata_from_eata(
                        input.ata_account.account_shared_data(),
                        eata_shared,
                        &deleg,
                    )
                {
                    account_to_clone = projected_ata;
                }
            }
        }

        accounts_to_clone.push(AccountCloneRequest {
            pubkey: input.ata_pubkey,
            account: account_to_clone,
            commit_frequency_ms,
            delegated_to_other,
        });
    }

    accounts_to_clone
}
