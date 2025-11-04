use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;
use crate::some_or_err;

impl WsDispatcher {
    /// Handles the `accountSubscribe` WebSocket RPC request.
    ///
    /// Registers the current WebSocket connection to receive notifications whenever
    /// the specified account is modified. The encoding of the notification can be
    /// customized via an optional configuration object. Returns the subscription ID
    /// used to identify notifications and to unsubscribe.
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
        let encoder =
            config.encoding.unwrap_or(UiAccountEncoding::Base58).into();

        // Register the subscription with the global database.
        let handle = self
            .subscriptions
            .subscribe_to_account(pubkey, encoder, self.chan.clone())
            .await;

        let result = SubResult::SubId(handle.id);
        // Store the cleanup handle to manage the subscription's lifecycle.
        self.register_unsub(handle);

        Ok(result)
    }
}
