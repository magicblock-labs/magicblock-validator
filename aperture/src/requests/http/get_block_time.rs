use magicblock_core::Slot;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload};

impl RpcHandlers {
    pub(crate) async fn get_block_time(&self, request: &JsonRequest) -> HandlerResult {
        let block = request.required::<Slot>(0)?;

        let block_time = self.history.block_time(block).await?;

        ResponsePayload::encode_no_context(&request.id, block_time)
    }
}
