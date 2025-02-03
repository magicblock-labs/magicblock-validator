// NOTE: from rpc/src/rpc.rs :2287 and rpc/src/rpc/account_resolver.rs
#![allow(dead_code)]
use std::collections::HashMap;

use jsonrpc_core::{error, Result};
use magicblock_bank::bank::Bank;
use magicblock_tokens::token_balances::get_mint_decimals_from_data;
use solana_account_decoder::{
    parse_account_data::AccountAdditionalData,
    parse_token::{get_token_account_mint, is_known_spl_token_id},
    UiAccount, UiAccountEncoding, UiDataSliceConfig, MAX_BASE58_BYTES,
};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    pubkey::Pubkey,
};

pub(crate) fn get_account_from_overwrites_or_bank(
    pubkey: &Pubkey,
    bank: &Bank,
    overwrite_accounts: Option<&HashMap<Pubkey, AccountSharedData>>,
) -> Option<AccountSharedData> {
    overwrite_accounts
        .and_then(|accounts| accounts.get(pubkey).cloned())
        .or_else(|| bank.get_account(pubkey))
}

pub(crate) fn get_encoded_account(
    bank: &Bank,
    pubkey: &Pubkey,
    encoding: UiAccountEncoding,
    data_slice: Option<UiDataSliceConfig>,
    // only used for simulation results
    overwrite_accounts: Option<&HashMap<Pubkey, AccountSharedData>>,
) -> Result<Option<UiAccount>> {
    match get_account_from_overwrites_or_bank(pubkey, bank, overwrite_accounts)
    {
        Some(account) => {
            let response = if is_known_spl_token_id(account.owner())
                && encoding == UiAccountEncoding::JsonParsed
            {
                get_parsed_token_account(
                    bank,
                    pubkey,
                    account,
                    overwrite_accounts,
                )
            } else {
                encode_account(&account, pubkey, encoding, data_slice)?
            };
            Ok(Some(response))
        }
        None => Ok(None),
    }
}

pub(crate) fn encode_account<T: ReadableAccount>(
    account: &T,
    pubkey: &Pubkey,
    encoding: UiAccountEncoding,
    data_slice: Option<UiDataSliceConfig>,
) -> Result<UiAccount> {
    if (encoding == UiAccountEncoding::Binary
        || encoding == UiAccountEncoding::Base58)
        && account.data().len() > MAX_BASE58_BYTES
    {
        let message = format!("Encoded binary (base 58) data should be less than {MAX_BASE58_BYTES} bytes, please use Base64 encoding.");
        Err(error::Error {
            code: error::ErrorCode::InvalidRequest,
            message,
            data: None,
        })
    } else {
        //Ok(UiAccount::encode(
        //    pubkey, account, encoding, None, data_slice,
        //))
        todo!()
    }
}

// -----------------
// Token Accounts
// -----------------
// NOTE: from rpc/src/parsed_token_accounts.rs
pub(crate) fn get_parsed_token_account(
    bank: &Bank,
    pubkey: &Pubkey,
    account: AccountSharedData,
    // only used for simulation results
    overwrite_accounts: Option<&HashMap<Pubkey, AccountSharedData>>,
) -> UiAccount {
    let additional_data = get_token_account_mint(account.data())
        .and_then(|mint_pubkey| {
            get_account_from_overwrites_or_bank(
                &mint_pubkey,
                bank,
                overwrite_accounts,
            )
        })
        .map(|mint_account| AccountAdditionalData {
            spl_token_decimals: get_mint_decimals_from_data(
                mint_account.data(),
            )
            .ok(),
        });

    todo!()
    //UiAccount::encode(
    //    pubkey,
    //    &account,
    //    UiAccountEncoding::JsonParsed,
    //    additional_data,
    //    None,
    //)
}
