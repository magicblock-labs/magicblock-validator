use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_slot(&self, request: &JsonRequest) -> HandlerResult {
        let slot = self.accountsdb.slot();
        Ok(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
