use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `isBlockhashValid` RPC request.
    ///
    /// Checks if a given blockhash is still valid. Validity is determined by the
    /// blockhash's presence in the validator's time-limited `BlocksCache`.
    pub(crate) fn is_blockhash_valid(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let blockhash_bytes = parse_params!(request.params()?, Serde32Bytes);
        let blockhash = some_or_err!(blockhash_bytes);

        let valid = self.blocks.contains(&blockhash);
        let slot = self.blocks.block_height();

        Ok(ResponsePayload::encode(&request.id, valid, slot))
    }
}
