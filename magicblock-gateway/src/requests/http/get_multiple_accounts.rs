use std::convert;

use hyper::Response;
use magicblock_gateway_types::accounts::LockedAccount;
use solana_account_decoder::{encode_ui_account, UiAccountEncoding};
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use crate::{
    error::RpcError,
    requests::{params::SerdePubkey, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) fn get_multiple_accounts(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (pubkeys, config) =
            parse_params!(params, Vec<SerdePubkey>, RpcAccountInfoConfig);
        let pubkeys = pubkeys.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        });
        unwrap!(pubkeys, request.id);
        let config = config.unwrap_or_default();
        let slot = self.accountsdb.slot();
        let reader = self.accountsdb.reader().map_err(RpcError::internal);
        unwrap!(reader, request.id);
        let mut accounts = Vec::with_capacity(pubkeys.len());
        for pubkey in pubkeys {
            let account =
                reader.read(&pubkey.0, convert::identity).map(|acc| {
                    let locked = LockedAccount::new(pubkey.0, acc);
                    locked.read_locked(|pk, acc| {
                        encode_ui_account(
                            pk,
                            acc,
                            config
                                .encoding
                                .unwrap_or(UiAccountEncoding::Base58),
                            None,
                            None,
                        )
                    })
                });
            accounts.push(account);
        }
        ResponsePayload::encode(&request.id, accounts, slot)
    }
}
