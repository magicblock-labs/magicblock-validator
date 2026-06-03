use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getMultipleAccounts` RPC request.
    ///
    /// Fetches a batch of accounts by their public keys. The encoding for
    /// accounts can be specified via an optional configuration object.
    ///
    /// The returned list has the same length as the input `pubkeys`
    /// list, with `null` entries for accounts that are not found.
    pub(crate) async fn get_multiple_accounts(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (pubkeys, config) = parse_params!(
            request.params()?,
            Vec<Serde32Bytes>,
            RpcAccountInfoConfig
        );

        let pubkeys: Vec<Serde32Bytes> = some_or_err!(pubkeys);
        let pubkeys: Vec<Pubkey> =
            pubkeys.into_iter().map(Into::into).collect();

        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;

        // Bundles each pubkey's permission PDA into the same chainlink ensure
        // when the request is authenticated, so query-filtering doesn't pay
        // an extra round-trip.
        #[cfg_attr(not(feature = "query-filtering"), allow(unused_mut))]
        let mut raw_accounts = self
            .read_accounts_with_ensure_for_user(
                &pubkeys,
                request.authenticated_user.as_ref(),
            )
            .await;
        #[cfg(feature = "query-filtering")]
        if let Some(user) = &request.authenticated_user {
            let permissions =
                magicblock_query_filtering::permissions_for_accounts(
                    &*self.accountsdb,
                    &pubkeys,
                )
                .map_err(RpcError::internal)?;
            raw_accounts = magicblock_query_filtering::filter_accounts(
                raw_accounts,
                &permissions,
                user,
            );
        }

        let accounts = pubkeys
            .iter()
            .zip(raw_accounts)
            .map(|(pubkey, acc)| {
                acc.filter(|account| {
                    !Self::account_should_render_as_null(account)
                })
                .map(|account| {
                    LockedAccount::new(*pubkey, account)
                        .ui_encode(encoding, slice)
                })
            })
            .collect::<Vec<_>>();

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, accounts, slot))
    }
}
