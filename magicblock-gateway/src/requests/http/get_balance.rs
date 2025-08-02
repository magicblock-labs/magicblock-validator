use hyper::Response;

use crate::{
    error::RpcError,
    requests::{params::Serde32Bytes, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) async fn get_balance(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, &request.id);
        let pubkey = parse_params!(params, Serde32Bytes);
        let pubkey = pubkey.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        });
        unwrap!(pubkey, &request.id);
        let slot = self.accountsdb.slot();
        let account = self.read_account_with_ensure(&pubkey).await;
        ResponsePayload::encode(
            &request.id,
            account.map(|a| a.lamports()).unwrap_or_default(),
            slot,
        )
    }
}
