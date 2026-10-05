use super::{HandlerResult, RpcHandlers};
use crate::requests::{
    JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload,
};

impl RpcHandlers {
    pub(crate) fn is_blockhash_valid(&self, request: &JsonRequest) -> HandlerResult {
        let blockhash: solana_hash::Hash = request.required::<Serde32Bytes>(0)?.into();

        let valid = self.engine.blocks().is_valid(&blockhash);
        let slot = self.engine.blocks().latest().slot;

        ResponsePayload::encode(&request.id, valid, slot)
    }
}
