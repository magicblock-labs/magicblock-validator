use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_block_height(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let slot = self.blocks.block_height();
        Response::new(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
