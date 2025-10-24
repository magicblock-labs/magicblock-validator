use super::{get_blocks_with_limit::MAX_DEFAULT_BLOCKS_LIMIT, prelude::*};

impl HttpDispatcher {
    /// Handles the `getBlocks` RPC request.
    ///
    /// Returns a list of slot numbers within a specified range.
    ///
    /// Note: This implementation returns a contiguous list of all slot
    /// numbers from the `start_slot` to the `end_slot` (or the latest slot
    ///  if `end_slot` is not provided) and does not confirm that a block
    /// was produced in each slot. This is due to the fact that ER validators
    /// never skip any slot numbers, and produce a block for each
    pub(crate) fn get_blocks(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (start_slot, end_slot) =
            parse_params!(request.params()?, Slot, Slot);
        let start_slot: Slot = some_or_err!(start_slot);

        let latest_slot = self.blocks.block_height();
        // If an end_slot is provided, cap it at the current latest_slot.
        // Otherwise, default to the latest_slot.
        let end_slot = end_slot
            .map(|end| end.min(latest_slot))
            .unwrap_or(latest_slot)
            .min(start_slot.saturating_add(MAX_DEFAULT_BLOCKS_LIMIT));

        if start_slot > end_slot {
            return Err(RpcError::invalid_params(
                "start slot is greater than the end slot",
            ));
        };

        let slots = (start_slot..=end_slot).collect::<Vec<Slot>>();
        Ok(ResponsePayload::encode_no_context(&request.id, slots))
    }
}
