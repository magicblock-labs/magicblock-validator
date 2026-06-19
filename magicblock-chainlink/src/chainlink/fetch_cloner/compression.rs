use borsh::BorshDeserialize;
use compressed_delegation_client::CompressedDelegationRecord;
use magicblock_accounts_db::traits::AccountsBank;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use tracing::*;

use super::FetchCloner;
use crate::{
    cloner::{AccountCloneRequest, Cloner, DelegationActions},
    remote_account_provider::{
        photon_client::PhotonClient, ChainPubsubClient, ChainRpcClient,
    },
};

/// Expand a [`CompressedDelegationRecord`] from `data` into the logical owner, payload, and
/// delegation flags.
///
/// Returns `None` when `data` is not a valid borsh record (including when the account is already
/// expanded and `data` holds only the program payload).
pub(crate) fn try_decompress_compressed_delegation<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    account: &AccountSharedData,
    min_context_slot: u64,
) -> Option<(AccountSharedData, u64)>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    let Ok(compressed_delegation_record) =
        CompressedDelegationRecord::try_from_slice(account.data())
    else {
        return None;
    };
    let effective_slot = min_context_slot.max(account.remote_slot());
    let delegation_slot = compressed_delegation_record.delegation_slot;
    let mut out = AccountSharedData::new(
        compressed_delegation_record.lamports,
        compressed_delegation_record.data.len(),
        &compressed_delegation_record.owner,
    );
    out.set_data(compressed_delegation_record.data);
    out.set_compressed(true);
    out.set_remote_slot(effective_slot);
    out.set_delegated(
        compressed_delegation_record.authority == this.validator_pubkey,
    );
    Some((out, delegation_slot))
}

#[instrument(skip(this, compressed_accounts))]
pub(crate) fn resolve_compressed_accounts<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    compressed_accounts: Vec<(Pubkey, AccountSharedData, u64)>,
    min_context_slot: Option<u64>,
) -> Vec<AccountCloneRequest>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    if compressed_accounts.is_empty() {
        return vec![];
    }

    let mut accounts_to_clone = Vec::with_capacity(compressed_accounts.len());

    for (pubkey, account_shared_data, slot) in compressed_accounts {
        let effective_slot = if let Some(min_slot) = min_context_slot {
            min_slot.max(slot)
        } else {
            slot
        };

        let Ok(compressed_delegation_record) =
            CompressedDelegationRecord::try_from_slice(
                account_shared_data.data(),
            )
        else {
            warn!(
                pubkey = %pubkey,
                "Failed to deserialize compressed delegation record"
            );
            continue;
        };

        let mut account = AccountSharedData::new(
            compressed_delegation_record.lamports,
            compressed_delegation_record.data.len(),
            &compressed_delegation_record.owner,
        );
        account.set_data(compressed_delegation_record.data);
        account.set_compressed(true);
        account.set_remote_slot(effective_slot);
        account.set_delegated(
            compressed_delegation_record.authority == this.validator_pubkey,
        );

        accounts_to_clone.push(AccountCloneRequest {
            pubkey,
            account,
            commit_frequency_ms: None,
            delegation_actions: DelegationActions::default(),
            delegated_to_other: None,
        });
    }

    accounts_to_clone
}
