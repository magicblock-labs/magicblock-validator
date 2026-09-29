use magicblock_metrics::metrics::AccountFetchEntrypoint;
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
                    AccountFetchEntrypoint::RpcGetAccount,
                    |account| account.is(AccountMode::Delegated),
                )
                .await;
            claims += remote_account_claims;

            let is_delegated = account.unwrap_or(false);

            let payload = json::json!({ "isDelegated": is_delegated });

            Ok(ResponsePayload::encode_no_context(&request.id, payload))
        }
        .await;
        (result, claims)
    }
}
