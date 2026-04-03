use std::sync::Arc;

use borsh::BorshDeserialize;
use compressed_delegation_client::CompressedDelegationRecord;
use dlp::state::DelegationRecord;
use magicblock_core::traits::AccountsBank;
use magicblock_metrics::metrics::AccountFetchOrigin;
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;
use tracing::*;

use crate::{
    chainlink::account_still_undelegating_on_chain::account_still_undelegating_on_chain,
    cloner::{AccountCloneRequest, Cloner},
    fetch_cloner::{types::RefreshDecision, FetchCloner},
    remote_account_provider::{
        photon_client::PhotonClient, ChainPubsubClient, ChainRpcClient,
        RemoteAccountProvider,
    },
};

pub(crate) async fn resolve_compressed_delegated_accounts<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    owned_by_deleg_compressed: Vec<(Pubkey, AccountSharedData, u64)>,
) -> Vec<AccountCloneRequest>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
{
    owned_by_deleg_compressed
        .into_iter()
        .filter_map(|(pubkey, mut account, _)| {
            match CompressedDelegationRecord::try_from_slice(account.data()) {
                Ok(delegation_record) => {
                    account.set_compressed(true);
                    account.set_lamports(delegation_record.lamports);
                    account.set_owner(delegation_record.owner);
                    account.set_data(delegation_record.data);
                    account.set_delegated(
                        delegation_record.authority.eq(&this.validator_pubkey),
                    );
                    account.set_confined(
                        delegation_record.authority.eq(&Pubkey::default()),
                    );

                    let delegated_to_other =
                        this.get_delegated_to_other(&DelegationRecord {
                            authority: delegation_record.authority,
                            owner: delegation_record.owner,
                            delegation_slot: delegation_record.delegation_slot,
                            lamports: delegation_record.lamports,
                            commit_frequency_ms: 0,
                        });

                    Some(AccountCloneRequest {
                        pubkey,
                        account,
                        commit_frequency_ms: None,
                        delegated_to_other,
                    })
                }
                Err(err) => {
                    error!(
                        pubkey = %pubkey,
                        error = %err,
                        account = ?account,
                        "Failed to deserialize compressed delegation record",
                    );
                    None
                }
            }
        })
        .collect::<Vec<_>>()
}

pub(crate) async fn resolve_compressed_accounts_to_clone<
    T: ChainRpcClient,
    U: ChainPubsubClient,
    P: PhotonClient,
>(
    pubkey: Pubkey,
    mut account: AccountSharedData,
    remote_account_provider: Arc<RemoteAccountProvider<T, U, P>>,
    validator_pubkey: Pubkey,
) -> (Option<AccountSharedData>, Option<DelegationRecord>) {
    // If the account is compressed, it can contain either:
    // 1. The delegation record
    // 2. No data in case the last update was the notif where the account was emptied
    // If we fail to get the record, we need to fetch again so that we obtain the data
    // from the compressed account.
    let delegation_record = match CompressedDelegationRecord::try_from_slice(
        account.data(),
    ) {
        Ok(delegation_record) => Some(delegation_record),
        Err(parse_err) => {
            debug!(
                pubkey = %pubkey,
                error = %parse_err,
                "The account's data did not contain a valid compressed delegation record",
            );
            match remote_account_provider
                .try_get(pubkey, AccountFetchOrigin::GetAccount)
                .await
            {
                Ok(remote_acc) => {
                    if let Some(acc) = remote_acc.fresh_account().cloned() {
                        match CompressedDelegationRecord::try_from_slice(
                            acc.data(),
                        ) {
                            Ok(delegation_record) => Some(delegation_record),
                            Err(parse_err) => {
                                error!(
                                    pubkey = %pubkey,
                                    error = %parse_err,
                                    "Fetched account parse failed",
                                );
                                None
                            }
                        }
                    } else {
                        error!(
                            pubkey = %pubkey,
                            "Remote fetch failed, no fresh account returned",
                        );
                        None
                    }
                }
                Err(fetch_err) => {
                    error!(
                        pubkey = %pubkey,
                        error = %fetch_err,
                        "Remote fetch failed",
                    );
                    None
                }
            }
        }
    };

    if let Some(delegation_record) = delegation_record {
        account.set_compressed(true);
        account.set_owner(delegation_record.owner);
        account.set_data(delegation_record.data);
        account.set_lamports(delegation_record.lamports);
        account
            .set_confined(delegation_record.authority.eq(&Pubkey::default()));

        let is_delegated_to_us =
            delegation_record.authority.eq(&validator_pubkey);
        account.set_delegated(is_delegated_to_us);

        // TODO(dode): commit frequency ms is not supported for compressed delegation records
        (
            Some(account),
            Some(DelegationRecord {
                authority: delegation_record.authority,
                owner: delegation_record.owner,
                delegation_slot: delegation_record.delegation_slot,
                lamports: delegation_record.lamports,
                commit_frequency_ms: 0,
            }),
        )
    } else {
        (None, None)
    }
}

pub(crate) async fn should_refresh_undelegating_in_bank_compressed_account(
    pubkey: &Pubkey,
    in_bank: &AccountSharedData,
    validator_pubkey: Pubkey,
) -> RefreshDecision {
    let Some(record) =
        CompressedDelegationRecord::try_from_slice(in_bank.data()).ok()
    else {
        // The delegation record is not present in the account data because the actual account data
        // has already been extracted from it. Refreshing would reset the account, losing local changes.
        debug!(
            pubkey = %pubkey,
            data = %format!("{:?}", in_bank.data()),
            "Skip refresh for already processed compressed account"
        );
        return RefreshDecision::No;
    };

    if !account_still_undelegating_on_chain(
        pubkey,
        record.authority.eq(&validator_pubkey)
            || record.authority.eq(&Pubkey::default()),
        in_bank.remote_slot(),
        Some(DelegationRecord {
            authority: record.authority,
            owner: record.owner,
            delegation_slot: record.delegation_slot,
            lamports: record.lamports,
            commit_frequency_ms: 0, // TODO(dode): use the actual commit frequency once implemented
        }),
        &validator_pubkey,
    ) {
        debug!(
            pubkey = %pubkey,
            "Refresh compressed account since the compressed delegation record is not undelegating"
        );
        return RefreshDecision::Yes;
    };
    debug!(
        pubkey = %pubkey,
        "Skip refresh for compressed account since the compressed delegation record is undelegating"
    );

    RefreshDecision::No
}
