use std::str::FromStr;

use hyper::Response;
use magicblock_gateway_types::accounts::{
    LockedAccount, Pubkey, ReadableAccount,
};
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcTokenAccountsFilter,
};

use crate::{
    error::RpcError,
    requests::{
        http::{SPL_MINT_OFFSET, SPL_OWNER_OFFSET, TOKEN_PROGRAM_ID},
        params::SerdePubkey,
        payload::ResponsePayload,
        JsonRequest,
    },
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::{AccountWithPubkey, JsonBody, ProgramFilter, ProgramFilters},
};

impl HttpDispatcher {
    pub(crate) fn get_token_accounts_by_owner(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (owner, filter, config) = parse_params!(
            params,
            SerdePubkey,
            RpcTokenAccountsFilter,
            RpcAccountInfoConfig
        );
        let owner = owner.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid owner")
        });
        unwrap!(owner, request.id);
        let filter = filter.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid filter")
        });
        unwrap!(filter, request.id);
        let config = config.unwrap_or_default();
        let slot = self.accountsdb.slot();
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
            offset: SPL_OWNER_OFFSET,
            bytes: owner.0.to_bytes().to_vec(),
        });
        let accounts = self
            .accountsdb
            .get_program_accounts(&program, move |a| filters.matches(a.data()))
            .map_err(RpcError::internal);
        unwrap!(accounts, request.id);
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;
        let accounts = accounts
            .into_iter()
            .map(|(pubkey, account)| {
                let locked = LockedAccount::new(pubkey, account);
                AccountWithPubkey::new(&locked, encoding, slice)
            })
            .collect::<Vec<_>>();
        ResponsePayload::encode(&request.id, accounts, slot)
    }
}
