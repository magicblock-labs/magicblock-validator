use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcProgramAccountsConfig;

use crate::{
    encoder::{AccountEncoder, ProgramAccountEncoder},
    error::RpcError,
    requests::{params::Serde32Bytes, JsonRequest},
    server::websocket::dispatch::{SubResult, WsDispatcher},
    utils::ProgramFilters,
    RpcResult,
};

impl WsDispatcher {
    pub(crate) async fn program_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let mut params = request
            .params
            .take()
            .ok_or_else(|| RpcError::invalid_request("missing params"))?;

        let (pubkey, config) =
            parse_params!(params, Serde32Bytes, RpcProgramAccountsConfig);
        let pubkey = pubkey.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        })?;
        let config = config.unwrap_or_default();
        let encoder: AccountEncoder = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58)
            .into();
        let filters = ProgramFilters::from(config.filters);
        let encoder = ProgramAccountEncoder { encoder, filters };
        let handle = self
            .subscriptions
            .subscribe_to_program(pubkey, encoder, self.chan.clone())
            .await;
        self.unsubs.insert(handle.id, handle.cleanup);
        Ok(SubResult::SubId(handle.id))
    }
}
