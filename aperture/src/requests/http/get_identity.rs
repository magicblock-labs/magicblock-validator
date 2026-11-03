use solana_rpc_client_api::response::RpcIdentity;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload};

impl RpcHandlers {
    pub(crate) fn get_identity(&self, request: &JsonRequest) -> HandlerResult {
        let identity = self.engine.authority().to_string();
        let response = RpcIdentity { identity };
        ResponsePayload::encode_no_context(&request.id, response)
    }
}
