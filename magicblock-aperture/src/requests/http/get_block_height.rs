use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getBlockHeight` RPC request.
    ///
    /// Returns the current block height of the validator, which is equivalent
    /// to the latest slot number from the `BlocksCache`.
    pub(crate) fn get_block_height(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let height = self.engine.blocks().latest().slot;
        Ok(ResponsePayload::encode_no_context(&request.id, height))
    }
}
