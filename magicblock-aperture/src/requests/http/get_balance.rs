use magicblock_metrics::metrics::AccountFetchContext;
use solana_account::ReadableAccount;
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
    pub(crate) async fn get_balance(
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
            let balance = account.map(|a| a.lamports()).unwrap_or_default();

            let slot = self.engine.blocks().latest().slot;
            Ok(ResponsePayload::encode(&request.id, balance, slot))
        }
        .await;
        (result, claims)
    }
}
