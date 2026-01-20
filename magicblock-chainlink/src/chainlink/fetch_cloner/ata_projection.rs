use magicblock_core::{
    token_programs::try_derive_eata_address_and_bump, traits::AccountsBank,
};
use magicblock_metrics::metrics;
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;
use tokio::task::JoinSet;
use tracing::*;

use super::{delegation, types::AccountWithCompanion, FetchCloner};
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
            warn!(pubkey = %ata_pubkey, error = %err, "Failed to subscribe to ATA");
        }
        if let Some((eata, _)) =
            try_derive_eata_address_and_bump(&ata_info.owner, &ata_info.mint)
        {
            if let Err(err) = this.subscribe_to_account(&eata).await {
                warn!(pubkey = %eata, error = %err, "Failed to subscribe to derived eATA");
            }

            let effective_slot = if let Some(min_slot) = min_context_slot {
                min_slot.max(*ata_account_slot)
            } else {
                *ata_account_slot
            };
            ata_join_set.spawn(FetchCloner::task_to_fetch_with_companion(
                this,
                *ata_pubkey,
                eata,
                effective_slot,
                fetch_origin,
            ));
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
                }
            }
        }

        accounts_to_clone.push(AccountCloneRequest {
            pubkey: ata_pubkey,
            account: account_to_clone,
            commit_frequency_ms,
            delegated_to_other,
        });
    }

    accounts_to_clone
}
