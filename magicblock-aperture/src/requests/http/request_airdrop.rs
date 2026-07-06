use super::HandlerResult;
use crate::{
    error::RpcError, requests::JsonHttpRequest as JsonRequest,
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) async fn request_airdrop(
        &self,
        _request: &JsonRequest,
    ) -> HandlerResult {
        Err(RpcError::invalid_request("free airdrop faucet is disabled"))
    }
}
