use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getDelegationStatus` RPC request.
    ///
    /// Returns a minimal delegation status object of the form:
    /// `{ "isDelegated": true | false }`
    ///
    /// The status is derived solely from the `AccountSharedData::delegated()`
    /// flag of the local `AccountsDb`. No delegation record resolution or
    /// router-style logic is performed here by design.
    pub(crate) async fn get_delegation_status(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        // Parse the first positional parameter (account pubkey) using the
        // standard helper macro, mirroring `get_account_info`.
        let pubkey = parse_params!(request.params()?, Serde32Bytes);
        let pubkey: Pubkey = some_or_err!(pubkey);

        // Ensure the account is present in the local AccountsDb, cloning it
        // from the reference cluster if necessary.
        let account = self.read_account_with_ensure(&pubkey).await;

        let is_delegated =
            account.as_ref().map(|acc| acc.delegated()).unwrap_or(false);

        let payload = json::json!({ "isDelegated": is_delegated });

        Ok(ResponsePayload::encode_no_context(&request.id, payload))
    }
}
