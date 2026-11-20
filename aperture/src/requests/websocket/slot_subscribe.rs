use super::prelude::*;
use crate::encoder::encode_slot;

impl WsDispatcher {
    pub(crate) async fn slot_subscribe(&mut self) -> RpcResult<SubResult> {
        let rx = self.engine.blocks().subscribe().await;
        Ok(self.forward(rx, |id, block| encode_slot(block.slot, id).map(Some)))
    }
}
