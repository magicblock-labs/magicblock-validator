use hyper::Response;

use crate::{
    error::RpcError,
    requests::{params::Serde32Bytes, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) fn is_blockhash_valid(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let blockhash = parse_params!(params, Serde32Bytes);
        let blockhash = blockhash.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid blockhash")
        });

        unwrap!(blockhash, request.id);
        let valid = self.blocks.contains(&blockhash);
        let slot = self.accountsdb.slot();
        ResponsePayload::encode(&request.id, valid, slot)
    }
}
