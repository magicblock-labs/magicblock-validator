use super::{HandlerResult, RpcHandlers};
use crate::requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload};

impl RpcHandlers {
    pub(crate) fn get_slot(&self, request: &JsonRequest) -> HandlerResult {
        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode_no_context(&request.id, slot)
    }
}
