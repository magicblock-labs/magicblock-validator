use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_slot(&self, request: &JsonRequest) -> HandlerResult {
        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
