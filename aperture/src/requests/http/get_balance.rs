use magicblock_metrics::metrics::AccountFetchEntrypoint;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{
    JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload,
};

impl RpcHandlers {
    pub(crate) async fn get_balance(
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
                |account| account.lamports(),
            )
            .await;
        let balance = account.unwrap_or_default();

        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode(&request.id, balance, slot)
    }
}
