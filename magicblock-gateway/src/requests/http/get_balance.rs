use hyper::Response;
use magicblock_accounts_db::AccountsDb;
use magicblock_gateway_types::accounts::ReadableAccount;

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
    unwrap!(mut params, &request.id);
    let pubkey = parse_params!(params, SerdePubkey);
    let pubkey = pubkey
        .ok_or_else(|| RpcError::invalid_params("missing or invalid pubkey"));
    unwrap!(pubkey, &request.id);
    let slot = accountsdb.slot();
    let Some(account) = accountsdb.get_account(&pubkey.0).ok() else {
        return ResponsePayload::encode(&request.id, None::<()>, slot);
    };
    ResponsePayload::encode(&request.id, account.lamports(), slot)
}
