use solana_rpc_client_api::response::RpcIdentity;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_identity(&self, request: &JsonRequest) -> HandlerResult {
        let identity = self.context.identity.to_string();
        let response = RpcIdentity { identity };
        Ok(ResponsePayload::encode_no_context(&request.id, response))
    }
}
