use hyper::Response;
use json::Serialize;
use magicblock_gateway_types::blocks::BlockHash;

use crate::{
    requests::{payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    utils::JsonBody,
    Slot,
};

impl HttpDispatcher {
    pub(crate) fn get_latest_blockhash(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let info = self.blocks.get_latest();
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct BlockHashResponse {
            blockhash: BlockHash,
            last_valid_block_height: Slot,
        }
        let response = BlockHashResponse {
            blockhash: info.hash,
            last_valid_block_height: info.validity,
        };
        ResponsePayload::encode(&request.id, response, info.slot)
    }
}
