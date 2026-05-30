use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getBalance` RPC request.
    ///
    /// Fetches the lamport balance for a given public key. If the account
    /// does not exist, it correctly returns a balance of `0`. The result is
    /// returned with the current slot context.
    pub(crate) async fn get_balance(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let pubkey_bytes = parse_params!(request.params()?, Serde32Bytes);
        let pubkey = some_or_err!(pubkey_bytes);

        // Bundles the permission PDA into the same chainlink ensure when the
        // request is authenticated.
        let mut balance = self
            .read_account_with_ensure_for_user(
                &pubkey,
                request.authenticated_user.as_ref(),
            )
            .await
            .map(|a| a.lamports())
            .unwrap_or_default(); // Default to 0 if account not found
        if let Some(user) = &request.authenticated_user {
            let permission =
                magicblock_query_filtering::permission_for_account(
                    &*self.accountsdb,
                    &pubkey,
                )
                .map_err(RpcError::internal)?;
            if !permission.access_for(user).account {
                balance = 0;
            }
        }

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, balance, slot))
    }
}
