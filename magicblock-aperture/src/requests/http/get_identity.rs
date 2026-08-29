use solana_rpc_client_api::response::RpcIdentity;

use super::HandlerResult;
use crate::{
    requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload},
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) fn get_identity(&self, request: &JsonRequest) -> HandlerResult {
        let identity = self.engine.authority().to_string();
        let response = RpcIdentity { identity };
        Ok(ResponsePayload::encode_no_context(&request.id, response))
    }
}
