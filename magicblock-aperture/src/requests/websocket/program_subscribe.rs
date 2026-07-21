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
    /// Spawns a task that forwards engine account updates for accounts owned by
    /// `pubkey` to this connection, applying the request's server-side filters and
    /// encoding. Returns the subscription ID.
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

        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);

        let filters = ProgramFilters::from(config.filters);
        let encoder = AccountEncoder {
            encoding,
            data_slice: config.account_config.data_slice,
        };
        let encoder = ProgramAccountEncoder { encoder, filters };

        let id = next_subid();
        let mut rx = self.engine.accounts().subscribe_program(pubkey).await;
        let tx = self.chan.tx.clone();
        let handle = tokio::spawn(async move {
            while let Ok((pubkey, account)) = rx.recv().await {
                let slot = account.slot();
                let Some(bytes) = encoder.encode(slot, &(pubkey, account), id)
                else {
                    continue;
                };
                if tx.send(bytes).await.is_err() {
                    break;
                }
            }
        });
        self.register(id, handle);

        Ok(SubResult::SubId(id))
    }
}
