use dlp::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use magicblock_core::{token_programs::EATA_PROGRAM_ID, traits::AccountsBank};
use magicblock_metrics::metrics;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use tracing::*;

use super::FetchCloner;
use crate::{
    chainlink::errors::{ChainlinkError, ChainlinkResult},
    cloner::Cloner,
    remote_account_provider::{
        photon_client::PhotonClient, ChainPubsubClient, ChainRpcClient,
        MatchSlotsConfig, ResolvedAccountSharedData,
    },
};

pub(crate) fn parse_delegation_record(
    data: &[u8],
    delegation_record_pubkey: Pubkey,
) -> ChainlinkResult<DelegationRecord> {
    DelegationRecord::try_from_bytes_with_discriminator(data)
        .copied()
        .map_err(|err| {
            ChainlinkError::InvalidDelegationRecord(
                delegation_record_pubkey,
                err,
            )
        })
}

pub(crate) fn apply_delegation_record_to_account<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    account: &mut ResolvedAccountSharedData,
    delegation_record: &DelegationRecord,
) -> Option<u64>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    let is_confined = delegation_record.authority.eq(&Pubkey::default());
    let is_delegated_to_us =
        delegation_record.authority.eq(&this.validator_pubkey) || is_confined;

    // Always update owner and confined flags
    account
        .set_owner(delegation_record.owner)
        .set_confined(is_confined);

    if is_delegated_to_us && delegation_record.owner != EATA_PROGRAM_ID {
        account.set_delegated(true);
    } else if !is_delegated_to_us {
        account.set_delegated(false);
    }
    if is_delegated_to_us {
        Some(delegation_record.commit_frequency_ms)
    } else {
        None
    }
}

pub(crate) fn get_delegated_to_other<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    delegation_record: &DelegationRecord,
) -> Option<Pubkey>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    let is_delegated_to_us =
        delegation_record.authority.eq(&this.validator_pubkey)
            || delegation_record.authority.eq(&Pubkey::default());

    (!is_delegated_to_us).then_some(delegation_record.authority)
}

#[instrument(skip(this))]
pub(crate) async fn fetch_and_parse_delegation_record<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    account_pubkey: Pubkey,
    min_context_slot: u64,
    fetch_origin: metrics::AccountFetchOrigin,
) -> Option<DelegationRecord>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    let delegation_record_pubkey =
        delegation_record_pda_from_delegated_account(&account_pubkey);
    let was_watching_deleg_record = this
        .remote_account_provider
        .is_watching(&delegation_record_pubkey);

    let res = match this
        .remote_account_provider
        .try_get_multi_until_slots_match(
            &[delegation_record_pubkey],
            Some(MatchSlotsConfig {
                min_context_slot: Some(min_context_slot),
                ..Default::default()
            }),
            fetch_origin,
        )
        .await
    {
        Ok(mut delegation_records) => {
            if let Some(delegation_record_remote) = delegation_records.pop() {
                match delegation_record_remote.fresh_account() {
                    Some(delegation_record_account) => {
                        FetchCloner::<T, U, V, C, P>::parse_delegation_record(
                            delegation_record_account.data(),
                            delegation_record_pubkey,
                        )
                        .ok()
                    }
                    None => None,
                }
            } else {
                None
            }
        }
        Err(_) => None,
    };

    if !was_watching_deleg_record
        // Handle edge case where it was cloned in the meantime.
        // The small possiblility of a fetch + clone of this delegation record being in process
        // still exits, but it's negligible
        && this
            .accounts_bank
            .get_account(&delegation_record_pubkey)
            .is_none()
    {
        // We only subscribed to fetch the delegation record, so unsubscribe now
        if let Err(err) = this
            .remote_account_provider
            .unsubscribe(&delegation_record_pubkey)
            .await
        {
            error!(pubkey = %delegation_record_pubkey, error = %err, "Failed to unsubscribe from delegation record");
        }
    }

    res
}
