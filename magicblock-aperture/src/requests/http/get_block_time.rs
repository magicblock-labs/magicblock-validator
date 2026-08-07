use ledger::request::{BlockDetails, BlockParams};
use magicblock_core::Slot;

use super::HandlerResult;
use crate::{
    error::{BLOCK_NOT_FOUND, RpcError},
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) async fn get_block_time(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let block = request.required::<Slot>(0)?;

        let engine_block = self
            .engine
            .blocks()
            .get(BlockParams {
                slot: block,
                details: BlockDetails::None,
            })
            .await
            .map_err(RpcError::internal)?;
        let block_time = if let Some(engine_block) = engine_block {
            engine_block.block().time
        } else {
            self.ledger.get_block_time(block)?.ok_or_else(|| {
                let error = format!(
                    "Slot {block} was skipped, or is not yet available"
                );
                RpcError::custom(error, BLOCK_NOT_FOUND)
            })?
        };

        Ok(ResponsePayload::encode_no_context(&request.id, block_time))
    }
}
