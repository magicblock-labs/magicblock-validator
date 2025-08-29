use super::prelude::*;

const MAX_DEFAULT_BLOCKS_LIMIT: u64 = 500_000;

impl HttpDispatcher {
    pub(crate) fn get_blocks_with_limit(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (start, limit) = parse_params!(request.params()?, Slot, Slot);
        let start: u64 = some_or_err!(start, "start slot");
        let limit = limit.unwrap_or(MAX_DEFAULT_BLOCKS_LIMIT);
        let end = (start + limit).min(self.blocks.block_height());
        let range = (start..end).collect::<Vec<Slot>>();

        Ok(ResponsePayload::encode_no_context(&request.id, range))
    }
}
