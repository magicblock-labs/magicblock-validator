use solana_rpc_client_api::response::RpcBlockhash;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_latest_blockhash(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        let info = self.blocks.get_latest();
        let slot = info.slot;
        let response = RpcBlockhash::from(info);
        Ok(ResponsePayload::encode(&request.id, response, slot))
    }
}
