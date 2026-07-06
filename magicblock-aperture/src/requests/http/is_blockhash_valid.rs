use super::HandlerResult;
use crate::{
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) fn is_blockhash_valid(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let blockhash: solana_hash::Hash =
            request.required::<Serde32Bytes>(0)?.into();

        let valid = self.engine.blocks().is_valid(&blockhash);
        let slot = self.engine.blocks().latest().slot;

        Ok(ResponsePayload::encode(&request.id, valid, slot))
    }
}
