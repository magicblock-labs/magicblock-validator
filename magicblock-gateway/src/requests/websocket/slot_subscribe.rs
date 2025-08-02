use crate::{
    server::websocket::dispatch::{SubResult, WsDispatcher},
    RpcResult,
};

impl WsDispatcher {
    pub(crate) fn slot_subscribe(&mut self) -> RpcResult<SubResult> {
        let handle = self.subscriptions.subscribe_to_slot(self.chan.clone());
        self.unsubs.insert(handle.id, handle.cleanup);
        Ok(SubResult::SubId(handle.id))
    }
}
