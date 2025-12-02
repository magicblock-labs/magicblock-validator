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
        // Parse single pubkey parameter from positional params without using
        // the generic `parse_params!` helper to keep the logic simple and
        // avoid tuple unpacking for this custom method.
        let params = request.params()?;
        // We expect the *first* positional parameter to be the account pubkey.
        let value = params.first().cloned().ok_or_else(|| {
            RpcError::invalid_params("missing pubkey parameter")
        })?;
        let pubkey_bytes: Serde32Bytes =
            json::from_value(&value).map_err(RpcError::parse_error)?;
        let pubkey: Pubkey = pubkey_bytes.into();

        // Ensure the account is present in the local AccountsDb, cloning it
        // from the reference cluster if necessary.
        let account = self.read_account_with_ensure(&pubkey).await;

        let is_delegated =
            account.as_ref().map(|acc| acc.delegated()).unwrap_or(false);

        let payload = json::json!({ "isDelegated": is_delegated });

        Ok(ResponsePayload::encode_no_context(&request.id, payload))
    }
}
