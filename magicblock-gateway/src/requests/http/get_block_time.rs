use crate::error::BLOCK_NOT_FOUND;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_block_time(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let block = parse_params!(request.params()?, Slot);
        let block = some_or_err!(block);

        let block = self.ledger.get_block(block)?.ok_or_else(|| {
            let error = format!(
                "Slot {block} was skipped, or missing in long-term message"
            );
            RpcError::custom(error, BLOCK_NOT_FOUND)
        })?;
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            block.block_time.unwrap_or_default(),
        ))
    }
}
