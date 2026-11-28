use ::metrics::metrics::AccountFetchEntrypoint;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::{HandlerResult, RpcHandlers};
use crate::{
    account::encoding::AccountEncoder,
    requests::{JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload},
};

impl RpcHandlers {
    pub(crate) async fn get_account_info(
        &self,
        request: &JsonRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        let pubkey: Pubkey = request.required::<Serde32Bytes>(0)?.into();
        let config = request.optional::<RpcAccountInfoConfig>(1)?.unwrap_or_default();
        self.accounts
            .ensure_one(&pubkey, AccountFetchEntrypoint::RpcGetAccount, claims)
            .await;
        let block = self.engine.blocks().latest();
        let account = AccountEncoder::from(config).read_visible(
            &self.engine.accounts().loader(),
            &pubkey,
            block.time,
        )?;
        ResponsePayload::encode(&request.id, account, block.slot)
    }
}
