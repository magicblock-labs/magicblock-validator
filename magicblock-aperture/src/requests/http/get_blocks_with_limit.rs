use super::prelude::*;

pub(crate) const MAX_DEFAULT_BLOCKS_LIMIT: u64 = 500_000;

impl HttpDispatcher {
    /// Handles the `getBlocksWithLimit` RPC request.
    ///
    /// Returns a list of slot numbers, starting from a
    /// given `start_slot` up to a specified `limit`.
    ///
    /// Note: ER validator produces a block in every slot, so this
    /// method returns a contiguous list of slot numbers.
    pub(crate) fn get_blocks_with_limit(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (start_slot, limit) = parse_params!(request.params()?, Slot, Slot);
        let start_slot: Slot = some_or_err!(start_slot);
        let limit = limit
            .unwrap_or(MAX_DEFAULT_BLOCKS_LIMIT)
            .min(MAX_DEFAULT_BLOCKS_LIMIT);
        let end_slot = start_slot + limit;
        // Calculate the end slot, ensuring it does not exceed the latest block height.
        let end_slot = (end_slot).min(self.blocks.block_height());

        // The range is exclusive of the end slot, so `(start..end)` is correct.
        let slots = (start_slot..end_slot).collect::<Vec<Slot>>();

        Ok(ResponsePayload::encode_no_context(&request.id, slots))
    }
}
