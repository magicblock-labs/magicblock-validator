use hyper::Response;
use magicblock_gateway_types::accounts::LockedAccount;
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use crate::{
    error::RpcError,
    requests::{params::Serde32Bytes, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) async fn get_account_info(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (pubkey, config) =
            parse_params!(params, Serde32Bytes, RpcAccountInfoConfig);
        let pubkey = pubkey.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        });
        unwrap!(pubkey, request.id);
        let config = config.unwrap_or_default();
        let slot = self.accountsdb.slot();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let account = self.read_account_with_ensure(&pubkey).await.map(|acc| {
            LockedAccount::new(pubkey, acc).ui_encode(encoding);
        });
        ResponsePayload::encode(&request.id, account, slot)
    }
}
