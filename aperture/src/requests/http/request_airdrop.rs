use super::{HandlerResult, RpcHandlers};
use crate::{error::RpcError, requests::JsonHttpRequest as JsonRequest};

impl RpcHandlers {
    pub(crate) async fn request_airdrop(&self, _request: &JsonRequest) -> HandlerResult {
        Err(RpcError::invalid_request("free airdrop faucet is disabled"))
    }
}
