use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getBlockhashForAccounts` RPC request.
    ///
    /// On the validator, this is intentionally implemented as an alias for
    /// `getLatestBlockhash`. The method exists purely for API compatibility
    /// with the Magic Router SDK and does not implement full router routing
    /// logic.
    pub(crate) fn get_blockhash_for_accounts(
        &self,
        request: &JsonRequest,
    ) -> HandlerResult {
        // We ignore any parameters and simply return the validator's latest
        // blockhash, matching the behavior of `getLatestBlockhash`.
        self.get_latest_blockhash(request)
    }
}
