use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_block_height(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
