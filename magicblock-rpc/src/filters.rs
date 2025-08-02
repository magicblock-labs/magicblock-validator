use jsonrpc_core::{Error, Result};
use log::*;
use magicblock_bank::bank::Bank;
use solana_account_decoder::parse_token::is_known_spl_token_id;
use solana_accounts_db::accounts_index::{
    AccountIndex, AccountSecondaryIndexes,
};
use solana_inline_spl::{
    token::SPL_TOKEN_ACCOUNT_OWNER_OFFSET, token_2022::ACCOUNTTYPE_ACCOUNT,
};
use solana_rpc::filter::filter_allows;
use solana_rpc_client_api::{
    custom_error::RpcCustomError, filter::RpcFilterType,
};
use solana_sdk::{
    account::AccountSharedData,
    pubkey::{Pubkey, PUBKEY_BYTES},
};
use spl_token_2022::{
    solana_program::program_pack::Pack, state::Account as TokenAccount,
};

use crate::RpcCustomResult;

pub(crate) fn optimize_filters(filters: &mut [RpcFilterType]) {
    filters.iter_mut().for_each(|filter_type| {
        if let RpcFilterType::Memcmp(compare) = filter_type {
            if let Err(err) = compare.convert_to_raw_bytes() {
                // All filters should have been previously verified
                warn!("Invalid filter: bytes could not be decoded, {err}");
            }
        }
    })
}

pub(crate) fn verify_filter(input: &RpcFilterType) -> Result<()> {
    input
        .verify()
        .map_err(|e| Error::invalid_params(format!("Invalid param: {e:?}")))
}

/// Analyze custom filters to determine if the result will be a subset of spl-token accounts by
/// owner.
/// NOTE: `optimize_filters()` should almost always be called before using this method because of
/// the strict match on `MemcmpEncodedBytes::Bytes`.
#[allow(unused)]
pub(crate) fn get_spl_token_owner_filter(
    program_id: &Pubkey,
    filters: &[RpcFilterType],
) -> Option<Pubkey> {
    if !is_known_spl_token_id(program_id) {
        return None;
    }
    let mut data_size_filter: Option<u64> = None;
    let mut memcmp_filter: Option<Vec<u8>> = None; // TODO optimize
    let mut owner_key: Option<Pubkey> = None;
    let mut incorrect_owner_len: Option<usize> = None;
    let mut token_account_state_filter = false;
    let account_packed_len = TokenAccount::get_packed_len();
    for filter in filters {
        match filter {
            RpcFilterType::DataSize(size) => data_size_filter = Some(*size),
            #[allow(deprecated)]
            RpcFilterType::Memcmp(mmcmp)
                if mmcmp.offset() == account_packed_len
                    && *program_id == solana_inline_spl::token_2022::id() =>
            {
                memcmp_filter =
                    Some(mmcmp.bytes().map(|b| b.to_vec()).unwrap_or_default())
            }
            #[allow(deprecated)]
            RpcFilterType::Memcmp(mmcmp)
                if mmcmp.offset() == SPL_TOKEN_ACCOUNT_OWNER_OFFSET =>
            {
                let bytes =
                    mmcmp.bytes().map(|b| b.to_vec()).unwrap_or_default();
                if bytes.len() == PUBKEY_BYTES {
                    owner_key = Pubkey::try_from(&bytes[..]).ok();
                } else {
                    incorrect_owner_len = Some(bytes.len());
                }
            }
            RpcFilterType::TokenAccountState => {
                token_account_state_filter = true
            }
            _ => {}
        }
    }
    if data_size_filter == Some(account_packed_len as u64)
        || memcmp_filter == Some([ACCOUNTTYPE_ACCOUNT].to_vec())
        || token_account_state_filter
    {
        if let Some(incorrect_owner_len) = incorrect_owner_len {
            info!(
                "Incorrect num bytes ({:?}) provided for spl_token_owner_filter",
                incorrect_owner_len
            );
        }
        owner_key
    } else {
        debug!(
            "spl_token program filters do not match by-owner index requisites"
        );
        None
    }
}

/// Use a set of filters to get an iterator of keyed program accounts from a bank
// we don't control solana_rpc_client_api::custom_error::RpcCustomError
#[allow(clippy::result_large_err)]
pub(crate) fn get_filtered_program_accounts(
    bank: &Bank,
    program_id: &Pubkey,
    account_indexes: &AccountSecondaryIndexes,
    mut filters: Vec<RpcFilterType>,
) -> RpcCustomResult<Vec<(Pubkey, AccountSharedData)>> {
    optimize_filters(&mut filters);
    let filter_closure = move |account: &AccountSharedData| {
        filters
            .iter()
            .all(|filter_type| filter_allows(filter_type, account))
    };
    if account_indexes.contains(&AccountIndex::ProgramId) {
        if !account_indexes.include_key(program_id) {
            return Err(RpcCustomError::KeyExcludedFromSecondaryIndex {
                index_key: program_id.to_string(),
            });
        }
        // NOTE: this used to use an account index based filter but we changed it to basically
        // be the same as the else branch
        Ok(
            bank.get_filtered_program_accounts(program_id, move |account| {
                // The program-id account index checks for Account owner on inclusion. However, due
                // to the current AccountsDb implementation, an account may remain in storage as a
                // zero-lamport AccountSharedData::Default() after being wiped and reinitialized in later
                // updates. We include the redundant filters here to avoid returning these
                // accounts.
                filter_closure(account)
            }),
        )
    } else {
        // this path does not need to provide a mb limit because we only want to support secondary indexes
        Ok(bank.get_filtered_program_accounts(program_id, filter_closure))
    }
}
