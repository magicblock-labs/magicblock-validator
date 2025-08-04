use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn is_blockhash_valid(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let blockhash = parse_params!(request.params()?, Serde32Bytes);
        let blockhash = blockhash.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid blockhash")
        })?;
        let valid = self.blocks.contains(&blockhash);
        let slot = self.accountsdb.slot();
        Ok(ResponsePayload::encode(&request.id, valid, slot))
    }
}
