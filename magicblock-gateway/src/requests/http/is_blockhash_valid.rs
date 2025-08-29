use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn is_blockhash_valid(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let blockhash = parse_params!(request.params()?, Serde32Bytes);
        let blockhash = some_or_err!(blockhash);
        let valid = self.blocks.contains(&blockhash);
        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, valid, slot))
    }
}
