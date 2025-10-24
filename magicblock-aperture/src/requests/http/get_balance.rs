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

        let balance = self
            .read_account_with_ensure(&pubkey)
            .await
            .map(|a| a.lamports())
            .unwrap_or_default(); // Default to 0 if account not found

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, balance, slot))
    }
}
