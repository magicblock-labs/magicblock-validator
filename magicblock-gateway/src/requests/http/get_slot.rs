use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_slot(&self, request: JsonRequest) -> Response<JsonBody> {
        let slot = self.accountsdb.slot();
        Response::new(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
