use super::prelude::*;
use crate::encoder::SlotEncoder;

impl WsDispatcher {
    /// Handles the `slotSubscribe` WebSocket RPC request.
    ///
    /// Spawns a task that forwards a notification to this connection each time the
    /// engine advances to a new block/slot.
    pub(crate) async fn slot_subscribe(&mut self) -> RpcResult<SubResult> {
        let id = next_subid();
        let mut rx = self.engine.blocks().subscribe();
        let tx = self.chan.tx.clone();
        let encoder = SlotEncoder;
        let handle = tokio::spawn(async move {
            while let Ok(block) = rx.recv().await {
                let Some(bytes) = encoder.encode(block.slot, &(), id) else {
                    continue;
                };
                if tx.send(bytes).await.is_err() {
                    break;
                }
            }
        });
        self.register(id, handle);

        Ok(SubResult::SubId(id))
    }
}
