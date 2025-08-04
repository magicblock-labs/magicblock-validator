use solana_rpc_client_api::response::RpcIdentity;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_identity(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let response = RpcIdentity {
            identity: self.identity.to_string(),
        };
        Response::new(ResponsePayload::encode_no_context(&request.id, response))
    }
}
