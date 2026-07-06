use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getSlot` RPC request.
    ///
    /// Returns the current slot of the validator from the `BlocksCache`.
    pub(crate) fn get_slot(&self, request: &JsonRequest) -> HandlerResult {
        let slot = self.engine.blocks().latest().slot;
        Ok(ResponsePayload::encode_no_context(&request.id, slot))
    }
}
