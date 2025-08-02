use hyper::Response;
use magicblock_gateway_types::accounts::{LockedAccount, ReadableAccount};
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcProgramAccountsConfig;

use crate::{
    error::RpcError,
    requests::{params::SerdePubkey, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::{AccountWithPubkey, JsonBody, ProgramFilters},
};

impl HttpDispatcher {
    pub(crate) fn get_program_accounts(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (program, config) =
            parse_params!(params, SerdePubkey, RpcProgramAccountsConfig);
        let program = program.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        });
        unwrap!(program, request.id);
        let config = config.unwrap_or_default();
        let filters = ProgramFilters::from(config.filters);
        let accounts = self
            .accountsdb
            .get_program_accounts(&program.0, move |a| {
                filters.matches(a.data())
            })
            .map_err(RpcError::internal);
        unwrap!(accounts, request.id);
        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);
        let slice = config.account_config.data_slice;
        let accounts = accounts
            .map(|(pubkey, account)| {
                let locked = LockedAccount::new(pubkey, account);
                AccountWithPubkey::new(&locked, encoding, slice)
            })
            .collect::<Vec<_>>();
        if config.with_context.unwrap_or_default() {
            let slot = self.accountsdb.slot();
            ResponsePayload::encode(&request.id, accounts, slot)
        } else {
            Response::new(ResponsePayload::encode_no_context(
                &request.id,
                accounts,
            ))
        }
    }
}
