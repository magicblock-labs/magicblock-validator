use super::HandlerResult;
use crate::{
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) fn get_slot(&self, request: &JsonRequest) -> HandlerResult {
        let slot = self.engine.blocks().latest().slot;
        Ok(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
