use solana_rpc_client_api::config::RpcAccountInfoConfig;
use tracing::*;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getAccountInfo` RPC request.
    ///
    /// Fetches an account by its public key, encodes it using the provided
    /// configuration, and returns it wrapped in a standard JSON-RPC response
    /// with the current slot context. Returns `null` if the account is not found.
    #[instrument(skip(self, request), fields(pubkey = tracing::field::Empty))]
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
        tracing::Span::current()
            .record("pubkey", tracing::field::display(&pubkey));
        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;

        debug!("Getting account info");

        // Bundles the permission PDA into the same chainlink ensure when the
        // request is authenticated so we don't pay a second round-trip just to
        // run query filtering.
        let mut account = self
            .read_account_with_ensure_for_user(
                &pubkey,
                request.authenticated_user.as_ref(),
            )
            .await
            .filter(|acc| !Self::account_should_render_as_null(acc))
            // `LockedAccount` provides a race-free read of the account data before encoding.
            .map(|acc| {
                LockedAccount::new(pubkey, acc).ui_encode(encoding, slice)
            });
        if let Some(user) = &request.authenticated_user {
            let permission =
                magicblock_query_filtering::permission_for_account(
                    &*self.accountsdb,
                    &pubkey,
                )
                .map_err(RpcError::internal)?;
            account = magicblock_query_filtering::filter_account(
                account,
                &permission,
                user,
            );
        }

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, account, slot))
    }
}
