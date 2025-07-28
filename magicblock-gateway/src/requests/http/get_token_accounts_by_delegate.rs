use std::str::FromStr;

use hyper::Response;
use magicblock_accounts_db::AccountsDb;
use magicblock_gateway_types::accounts::{Pubkey, ReadableAccount};
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcTokenAccountsFilter,
};

use crate::{
    error::RpcError,
    requests::{
        http::{SPL_DELEGATE_OFFSET, SPL_MINT_OFFSET, TOKEN_PROGRAM_ID},
        params::SerdePubkey,
        payload::ResponsePayload,
        JsonRequest,
    },
    unwrap,
    utils::{AccountWithPubkey, JsonBody, ProgramFilter, ProgramFilters},
};

pub(crate) fn handle(
    request: JsonRequest,
    accountsdb: &AccountsDb,
) -> Response<JsonBody> {
    let params = request
        .params
        .ok_or_else(|| RpcError::invalid_request("missing params"));
    unwrap!(mut params, request.id);
    let (delegate, filter, config) = parse_params!(
        params,
        SerdePubkey,
        RpcTokenAccountsFilter,
        RpcAccountInfoConfig
    );
    let delegate = delegate
        .ok_or_else(|| RpcError::invalid_params("missing or invalid owner"));
    unwrap!(delegate, request.id);
    let filter = filter
        .ok_or_else(|| RpcError::invalid_params("missing or invalid filter"));
    unwrap!(filter, request.id);
    let config = config.unwrap_or_default();
    let slot = accountsdb.slot();
    let mut filters = ProgramFilters::default();
    let mut program = TOKEN_PROGRAM_ID;
    match filter {
        RpcTokenAccountsFilter::Mint(pubkey) => {
            let bytes = bs58::decode(pubkey)
                .into_vec()
                .map_err(RpcError::parse_error);
            unwrap!(bytes, request.id);
            let filter = ProgramFilter::MemCmp {
                offset: SPL_MINT_OFFSET,
                bytes,
            };
            filters.push(filter);
        }
        RpcTokenAccountsFilter::ProgramId(pubkey) => {
            let pubkey =
                Pubkey::from_str(&pubkey).map_err(RpcError::parse_error);
            unwrap!(pubkey, request.id);
            program = pubkey;
        }
    };
    filters.push(ProgramFilter::MemCmp {
        offset: SPL_DELEGATE_OFFSET,
        bytes: delegate.0.to_bytes().to_vec(),
    });
    let accounts = accountsdb
        .get_program_accounts(&program, |a| filters.matches(a.data()))
        .map_err(RpcError::internal);
    unwrap!(accounts, request.id);
    let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
    let slice = config.data_slice;
    let accounts = accounts
        .into_iter()
        .map(|(pubkey, account)| {
            AccountWithPubkey::new(pubkey, &account, encoding, slice)
        })
        .collect::<Vec<_>>();
    ResponsePayload::encode(&request.id, accounts, slot)
}
