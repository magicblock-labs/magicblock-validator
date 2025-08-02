use hyper::Response;

use crate::{
    error::RpcError,
    requests::{payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
    Slot,
};

const MAX_DEFAULT_BLOCKS_LIMIT: u64 = 500_000;

impl HttpDispatcher {
    pub(crate) fn get_blocks_with_limit(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (start, limit) = parse_params!(params, Slot, Slot);
        let start =
            start.ok_or_else(|| RpcError::invalid_params("missing start slot"));
        unwrap!(start, request.id);
        let limit = limit.unwrap_or(MAX_DEFAULT_BLOCKS_LIMIT);
        let slot = self.accountsdb.slot();
        let end = (start + limit).min(slot);
        let range = (start..=end).collect::<Vec<Slot>>();
        Response::new(ResponsePayload::encode_no_context(&request.id, range))
    }
}
