use super::prelude::*;
use crate::encoder::encode_slot;

impl WsDispatcher {
    pub(crate) async fn slot_subscribe(&mut self) -> RpcResult<SubResult> {
        let id = next_subid();
        let rx = self.engine.blocks().subscribe().await;
        self.forward(id, rx, move |block| encode_slot(block.slot, id).map(Some));

        Ok(SubResult::SubId(id))
    }
}
