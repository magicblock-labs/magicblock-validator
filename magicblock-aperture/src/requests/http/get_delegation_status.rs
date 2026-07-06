use magicblock_metrics::metrics::AccountFetchContext;
use solana_account::AccountMode;
use solana_pubkey::Pubkey;

use super::ClaimedHandlerResult;
use crate::{
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) async fn get_delegation_status(
        &self,
        request: &JsonRequest,
    ) -> ClaimedHandlerResult {
        let mut claims = 0;
        let result = async {
            let pubkey: Pubkey = request.required::<Serde32Bytes>(0)?.into();

            let (account, remote_account_claims) = self
                .read_account_with_ensure(
                    &pubkey,
                    AccountFetchContext::rpc_get_account(),
                )
                .await;
            claims += remote_account_claims;

            let is_delegated = account
                .as_ref()
                .map(|acc| acc.is(AccountMode::Delegated))
                .unwrap_or(false);

            let payload = json::json!({ "isDelegated": is_delegated });

            Ok(ResponsePayload::encode_no_context(&request.id, payload))
        }
        .await;
        (result, claims)
    }
}
