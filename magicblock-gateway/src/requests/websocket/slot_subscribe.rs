use super::prelude::*;

impl WsDispatcher {
    /// Handles the `slotSubscribe` WebSocket RPC request.
    ///
    /// Registers the current WebSocket connection to receive a notification
    /// each time the validator advances to a new slot.
    pub(crate) fn slot_subscribe(&mut self) -> RpcResult<SubResult> {
        let handle = self.subscriptions.subscribe_to_slot(self.chan.clone());
        self.unsubs.insert(handle.id, handle.cleanup);
        Ok(SubResult::SubId(handle.id))
    }
}
