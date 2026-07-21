use solana_account::AccountSharedData;
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;
use crate::{encoder::AccountEncoder, some_or_err};

impl WsDispatcher {
    /// Handles the `accountSubscribe` WebSocket RPC request.
    ///
    /// Spawns a task that forwards engine account updates for `pubkey` to this
    /// connection, encoded per the request configuration. Returns the
    /// subscription ID used to identify notifications and to unsubscribe.
    pub(crate) async fn account_subscribe(
        &mut self,
        request: &mut JsonRequest,
    ) -> RpcResult<SubResult> {
        let (pubkey, config) = parse_params!(
            request.params()?,
            Serde32Bytes,
            RpcAccountInfoConfig
        );

        let pubkey = some_or_err!(pubkey);
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let encoder = AccountEncoder {
            encoding,
            data_slice: config.data_slice,
        };

        let id = next_subid();
        let mut rx = self.engine.accounts().subscribe(pubkey).await;
        let tx = self.chan.tx.clone();
        let handle = tokio::spawn(async move {
            while let Ok(account) = rx.recv().await {
                let account: AccountSharedData = account;
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
