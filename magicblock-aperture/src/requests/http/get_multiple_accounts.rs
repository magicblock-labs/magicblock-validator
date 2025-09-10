use std::convert::identity;

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

        let pubkeys: Vec<_> = some_or_err!(pubkeys);
        // SAFETY: Pubkey has the same memory layout and size as Serde32Bytes
        let pubkeys: Vec<Pubkey> = unsafe { std::mem::transmute(pubkeys) };

        let config = config.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);

        let mut accounts = vec![None; pubkeys.len()];
        // TODO(thlorenz): use chainlink
        let reader = self.accountsdb.reader()?;
        for (pubkey, account_slot) in pubkeys.iter().zip(&mut accounts) {
            *account_slot = reader.read(pubkey, identity).map(|acc| {
                LockedAccount::new(*pubkey, acc).ui_encode(encoding)
            });
        }

        // This collects pubkeys for accounts that were not found in the cache,
        // intended for a future implementation that would then ensure they are
        // loaded from primary storage.
        // TODO(thlorenz): use chainlink
        let _to_ensure: Vec<_> = accounts
            .iter()
            .zip(&pubkeys)
            .filter_map(|(acc, pk)| acc.is_none().then_some(*pk))
            .collect();

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, accounts, slot))
    }
}
