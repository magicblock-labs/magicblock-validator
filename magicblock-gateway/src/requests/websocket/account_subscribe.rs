use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use crate::{
    error::RpcError,
    requests::{params::SerdePubkey, JsonRequest},
    server::websocket::dispatch::{SubResult, WsDispatcher},
    RpcResult,
};

impl WsDispatcher {
    pub(crate) async fn account_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let mut params = request
            .params
            .take()
            .ok_or_else(|| RpcError::invalid_request("missing params"))?;

        let (pubkey, config) =
            parse_params!(params, SerdePubkey, RpcAccountInfoConfig);
        let pubkey = pubkey.ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        })?;
        let config = config.unwrap_or_default();
        let encoder =
            config.encoding.unwrap_or(UiAccountEncoding::Base58).into();
        let handle = self
            .subscriptions
            .subscribe_to_account(pubkey.0, encoder, self.chan.clone())
            .await;
        self.unsubs.insert(handle.id, handle.cleanup);
        Ok(SubResult::SubId(handle.id))
    }
}
