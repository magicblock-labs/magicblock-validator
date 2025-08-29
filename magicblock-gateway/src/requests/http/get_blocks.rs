use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_blocks(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (start, end) = parse_params!(request.params()?, Slot, Slot);
        let start = some_or_err!(start, "start slot");
        let slot = self.blocks.block_height();
        let end = end.map(|end| end.min(slot)).unwrap_or(slot);
        if start > end {
            Err(RpcError::invalid_params(
                "start slot is greater than the end slot",
            ))?;
        };
        let range = (start..=end).collect::<Vec<Slot>>();
        Ok(ResponsePayload::encode_no_context(&request.id, range))
    }
}
