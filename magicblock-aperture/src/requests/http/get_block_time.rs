use ledger::request::{BlockDetails, BlockParams};

use super::prelude::*;
use crate::error::BLOCK_NOT_FOUND;

impl HttpDispatcher {
    /// Handles the `getBlockTime` RPC request.
    ///
    /// Returns the estimated production time of a block, as a Unix timestamp.
    /// If the block is not found in the ledger (e.g., the slot was skipped),
    /// this method returns a `BLOCK_NOT_FOUND` error.
    pub(crate) async fn get_block_time(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let block = parse_params!(request.params()?, Slot);
        let block = some_or_err!(block);

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
            self.ledger
                .get_block(block)?
                .ok_or_else(|| {
                    let error = format!(
                        "Slot {block} was skipped, or is not yet available"
                    );
                    RpcError::custom(error, BLOCK_NOT_FOUND)
                })?
                .block_time
                .unwrap_or_default()
        };

        Ok(ResponsePayload::encode_no_context(&request.id, block_time))
    }
}
