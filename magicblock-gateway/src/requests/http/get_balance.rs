use hyper::Response;
use magicblock_gateway_types::accounts::ReadableAccount;

use crate::{
    error::RpcError,
    requests::{params::SerdePubkey, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) fn get_balance(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, &request.id);
        let pubkey = parse_params!(params, SerdePubkey);
        let pubkey = pubkey.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        });
        unwrap!(pubkey, &request.id);
        let slot = self.accountsdb.slot();
        let Some(account) = self.accountsdb.get_account(&pubkey.0).ok() else {
            return ResponsePayload::encode(&request.id, None::<()>, slot);
        };
        ResponsePayload::encode(&request.id, account.lamports(), slot)
    }
}
