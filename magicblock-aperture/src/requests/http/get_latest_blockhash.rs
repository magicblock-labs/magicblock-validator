use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getLatestBlockhash` RPC request.
    ///
    /// Returns the most recent blockhash from the engine and the last valid
    /// block height at which it can be used.
    pub(crate) fn get_latest_blockhash(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let (response, slot) = self.latest_blockhash();
        Ok(ResponsePayload::encode(&request.id, response, slot))
    }
}
