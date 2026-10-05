use magicblock_metrics::metrics::AccountFetchEntrypoint;
use solana_account::AccountMode;
use solana_pubkey::Pubkey;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{
    JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload,
};

impl RpcHandlers {
    pub(crate) async fn get_delegation_status(
        &self,
        request: &JsonRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        let pubkey: Pubkey = request.required::<Serde32Bytes>(0)?.into();

        let account = self
            .accounts
            .read(
                &pubkey,
                AccountFetchEntrypoint::RpcGetAccount,
                claims,
                |account| account.is(AccountMode::Delegated),
            )
            .await;

        let is_delegated = account.unwrap_or(false);

        let payload = json::json!({ "isDelegated": is_delegated });

        ResponsePayload::encode_no_context(&request.id, payload)
    }
}
