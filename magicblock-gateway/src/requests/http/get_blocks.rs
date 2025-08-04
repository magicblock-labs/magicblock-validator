use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_blocks(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (start, end) = parse_params!(request.params()?, Slot, Slot);
        let start = start
            .ok_or_else(|| RpcError::invalid_params("missing start slot"))?;
        let slot = self.accountsdb.slot();
        let end = end.map(|end| end.min(slot)).unwrap_or(slot);
        (start < end).then_some(()).ok_or_else(|| {
            RpcError::invalid_params("start slot is greater than the end slot")
        })?;
        let range = (start..=end).collect::<Vec<Slot>>();
        Ok(ResponsePayload::encode_no_context(&request.id, range))
    }
}
