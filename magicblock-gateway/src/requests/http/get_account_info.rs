use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getAccountInfo` RPC request.
    ///
    /// Fetches an account by its public key, encodes it using the provided
    /// configuration, and returns it wrapped in a standard JSON-RPC response
    /// with the current slot context. Returns `null` if the account is not found.
    pub(crate) async fn get_account_info(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (pubkey, config) = parse_params!(
            request.params()?,
            Serde32Bytes,
            RpcAccountInfoConfig
        );

        let pubkey: Pubkey = some_or_err!(pubkey);
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);

        // `read_account_with_ensure` guarantees the account is clone from chain if not in database.
        let account = self
            .read_account_with_ensure(&pubkey)
            .await
            // `LockedAccount` provides a race-free read of the account data before encoding.
            .map(|acc| LockedAccount::new(pubkey, acc).ui_encode(encoding));

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, account, slot))
    }
}
