use hyper::Response;

use crate::{
    error::RpcError,
    requests::{payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
    Slot,
};

impl HttpDispatcher {
    pub(crate) fn get_blocks(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (start, end) = parse_params!(params, Slot, Slot);
        let start =
            start.ok_or_else(|| RpcError::invalid_params("missing start slot"));
        unwrap!(start, request.id);
        let slot = self.accountsdb.slot();
        let end = end.map(|end| end.min(slot)).unwrap_or(slot);
        let _check = (start < end).then_some(()).ok_or_else(|| {
            RpcError::invalid_params("start slot is greater than the end slot")
        });
        unwrap!(_check, request.id);
        let range = (start..=end).collect::<Vec<Slot>>();
        Response::new(ResponsePayload::encode_no_context(&request.id, range))
    }
}
