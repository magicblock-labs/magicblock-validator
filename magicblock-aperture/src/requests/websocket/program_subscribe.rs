use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcProgramAccountsConfig;

use super::prelude::*;
use crate::{
    encoder::{AccountEncoder, ProgramAccountEncoder},
    some_or_err,
    utils::ProgramFilters,
};

impl WsDispatcher {
    /// Handles the `programSubscribe` WebSocket RPC request.
    ///
    /// Registers the current WebSocket connection to receive notifications for all
    /// accounts owned by the specified program. The stream of notifications can be
    /// refined using server-side data filters and a custom data encoding, provided
    /// in an optional configuration object.
    pub(crate) async fn program_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let (pubkey, config) = parse_params!(
            request.params()?,
            Serde32Bytes,
            RpcProgramAccountsConfig
        );

        let pubkey = some_or_err!(pubkey);
        let config = config.unwrap_or_default();

        let encoder: AccountEncoder = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58)
            .into();
        let filters = ProgramFilters::from(config.filters);

        // Bundle the encoding and filtering options for the subscription.
        let encoder = ProgramAccountEncoder { encoder, filters };

        let handle = self
            .subscriptions
            .subscribe_to_program(pubkey, encoder, self.chan.clone())
            .await;

        self.unsubs.insert(handle.id, handle.cleanup);
        Ok(SubResult::SubId(handle.id))
    }
}
