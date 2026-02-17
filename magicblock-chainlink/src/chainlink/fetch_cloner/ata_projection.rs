use std::sync::atomic::AtomicU16;

use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::{
    logger::log_trace_warn, token_programs::try_derive_eata_address_and_bump,
};
use magicblock_metrics::metrics;
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;
use tracing::*;

use super::{
    account_token_amount, delegation, types::AccountWithCompanion, FetchCloner,
};
use crate::{
    cloner::{AccountCloneRequest, Cloner},
    remote_account_provider::{ChainPubsubClient, ChainRpcClient},
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

    // Subscribe first so subsequent fetches are kept up-to-date
    for (ata_pubkey, _, ata_info, ata_account_slot) in &atas {
        if let Err(err) = this.subscribe_to_account(ata_pubkey).await {
            static ATA_SUBSCRIPTION_FAILURE_COUNT: AtomicU16 =
                AtomicU16::new(0);
            log_trace_warn(
                "Failed to subscribe to ATA",
                "Failed to subscribe to ATAs",
                &ata_pubkey,
                &err,
                1000,
                &ATA_SUBSCRIPTION_FAILURE_COUNT,
            );
        }
        if let Some((eata, _)) =
            try_derive_eata_address_and_bump(&ata_info.owner, &ata_info.mint)
        {
            if let Err(err) = this.subscribe_to_account(&eata).await {
                static EATA_SUBSCRIPTION_FAILURE_COUNT: AtomicU16 =
                    AtomicU16::new(0);
                log_trace_warn(
                    "Failed to subscribe to derived eATA",
                    "Failed to subscribe to derived eATAs",
                    &eata,
                    &err,
                    1000,
                    &EATA_SUBSCRIPTION_FAILURE_COUNT,
                );
            }

            let effective_slot = if let Some(min_slot) = min_context_slot {
                min_slot.max(*ata_account_slot)
            } else {
                *ata_account_slot
            };
            info!(
                ata_pubkey = %ata_pubkey,
                eata_pubkey = %eata,
                wallet_owner = %ata_info.owner,
                mint = %ata_info.mint,
                ata_slot = *ata_account_slot,
                effective_slot,
                "Mapped ATA to eATA for projection"
            );
            ata_join_set.spawn(FetchCloner::task_to_fetch_with_companion(
                this,
                *ata_pubkey,
                eata,
                effective_slot,
                fetch_origin,
            ));
        } else {
            debug!(
                ata_pubkey = %ata_pubkey,
                wallet_owner = %ata_info.owner,
                mint = %ata_info.mint,
                "Failed to derive eATA for ATA, cloning ATA as-is"
            );
        }
    }

    let ata_results = ata_join_set.join_all().await;

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

        // Defaults: clone the ATA as-is
        let mut account_to_clone = ata_account.account_shared_data_cloned();
        let mut commit_frequency_ms = None;
        let mut delegated_to_other = None;
        let mut projected_from_eata = false;

        // If there's an eATA, try to use it + delegation record to project the ATA
        if let Some(eata_acc) = maybe_eata_account {
            let eata_shared = eata_acc.account_shared_data_cloned();

            if let Some(deleg) = delegation::fetch_and_parse_delegation_record(
                this,
                eata_pubkey,
                this.remote_account_provider.chain_slot(),
                fetch_origin,
            )
            .await
            {
                delegated_to_other =
                    delegation::get_delegated_to_other(this, &deleg);
                commit_frequency_ms = Some(deleg.commit_frequency_ms);

                if let Some(projected_ata) = this
                    .maybe_project_delegated_ata_from_eata(
                        ata_account.account_shared_data(),
                        &eata_shared,
                        &deleg,
                    )
                {
                    account_to_clone = projected_ata;
                    projected_from_eata = true;
                    info!(
                        ata_pubkey = %ata_pubkey,
                        eata_pubkey = %eata_pubkey,
                        projected_slot = account_to_clone.remote_slot(),
                        commit_frequency_ms = deleg.commit_frequency_ms,
                        delegated_to_other = ?delegated_to_other,
                        eata_amount = ?account_token_amount(&eata_shared),
                        eata_delegated = eata_shared.delegated(),
                        ata_amount = ?account_token_amount(&account_to_clone),
                        ata_delegated = account_to_clone.delegated(),
                        "Projected ATA from delegated eATA"
                    );
                } else {
                    debug!(
                        ata_pubkey = %ata_pubkey,
                        eata_pubkey = %eata_pubkey,
                        "Delegation record found for eATA but ATA projection did not apply"
                    );
                }
            } else {
                debug!(
                    ata_pubkey = %ata_pubkey,
                    eata_pubkey = %eata_pubkey,
                    "No delegation record found for eATA, cloning ATA as-is"
                );
            }
        } else {
            debug!(
                ata_pubkey = %ata_pubkey,
                eata_pubkey = %eata_pubkey,
                "eATA account not found, cloning ATA as-is"
            );
        }

        let clone_mode = if projected_from_eata {
            "projected_from_eata"
        } else {
            "raw_ata"
        };
        info!(
            ata_pubkey = %ata_pubkey,
            eata_pubkey = %eata_pubkey,
            clone_mode,
            delegated = account_to_clone.delegated(),
            remote_slot = account_to_clone.remote_slot(),
            commit_frequency_ms = ?commit_frequency_ms,
            delegated_to_other = ?delegated_to_other,
            "Prepared ATA clone request"
        );
        accounts_to_clone.push(AccountCloneRequest {
            pubkey: ata_pubkey,
            account: account_to_clone,
            commit_frequency_ms,
            delegated_to_other,
        });
    }

    accounts_to_clone
}
