use hyper::Response;
use solana_rpc_client_api::response::RpcBlockhash;

use crate::{
    requests::{payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) fn get_latest_blockhash(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let info = self.blocks.get_latest();
        let slot = info.slot;
        let response = RpcBlockhash::from(info);
        ResponsePayload::encode(&request.id, response, slot)
    }
}
