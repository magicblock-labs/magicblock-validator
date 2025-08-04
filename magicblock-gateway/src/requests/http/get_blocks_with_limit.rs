use super::prelude::*;

const MAX_DEFAULT_BLOCKS_LIMIT: u64 = 500_000;

impl HttpDispatcher {
    pub(crate) fn get_blocks_with_limit(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (start, limit) = parse_params!(request.params()?, Slot, Slot);
        let start = start
            .ok_or_else(|| RpcError::invalid_params("missing start slot"))?;
        let limit = limit.unwrap_or(MAX_DEFAULT_BLOCKS_LIMIT);
        let slot = self.accountsdb.slot();
        let end = (start + limit).min(slot);
        let range = (start..=end).collect::<Vec<Slot>>();
        Ok(ResponsePayload::encode_no_context(&request.id, range))
    }
}
