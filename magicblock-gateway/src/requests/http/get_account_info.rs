use hyper::Response;
use magicblock_accounts_db::AccountsDb;
use solana_account_decoder::{encode_ui_account, UiAccountEncoding};
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use crate::{
    error::RpcError,
    requests::{params::SerdePubkey, payload::ResponsePayload, JsonRequest},
    unwrap,
    utils::JsonBody,
};

pub(crate) fn handle(
    request: JsonRequest,
    accountsdb: &AccountsDb,
) -> Response<JsonBody> {
    let params = request
        .params
        .ok_or_else(|| RpcError::invalid_request("missing params"));
    unwrap!(mut params, request.id);
    let (pubkey, config) =
        parse_params!(params, SerdePubkey, RpcAccountInfoConfig);
    let pubkey = pubkey
        .ok_or_else(|| RpcError::invalid_params("missing or invalid pubkey"));
    unwrap!(pubkey, request.id);
    let config = config.unwrap_or_default();
    let slot = accountsdb.slot();
    let Some(account) = accountsdb.get_account(&pubkey.0).ok() else {
        return ResponsePayload::encode(&request.id, None::<()>, slot);
    };
    let account = encode_ui_account(
        &pubkey.0,
        &account,
        config.encoding.unwrap_or(UiAccountEncoding::Base58),
        None,
        None,
    );
    ResponsePayload::encode(&request.id, account, slot)
}
