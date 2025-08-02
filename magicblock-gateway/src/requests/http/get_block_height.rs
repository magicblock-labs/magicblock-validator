use hyper::Response;

use crate::{
    requests::{payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) fn get_block_height(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let slot = self.blocks.block_height();
        Response::new(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
