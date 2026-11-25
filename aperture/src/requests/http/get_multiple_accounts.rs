use ::metrics::metrics::AccountFetchEntrypoint;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{config::RpcAccountInfoConfig, request::MAX_MULTIPLE_ACCOUNTS};

use super::{HandlerResult, RpcHandlers};
use crate::{
    account::encoding::AccountEncoder,
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload},
};

impl RpcHandlers {
    pub(crate) async fn get_multiple_accounts(
        &self,
        request: &JsonRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        let pubkeys = request.required::<Vec<Serde32Bytes>>(0)?;
        if pubkeys.len() > MAX_MULTIPLE_ACCOUNTS {
            return Err(RpcError::invalid_params(format!(
                "Too many accounts provided; max {MAX_MULTIPLE_ACCOUNTS}"
            )));
        }
        let pubkeys: Vec<Pubkey> = pubkeys.into_iter().map(Into::into).collect();

        let config = request.optional::<RpcAccountInfoConfig>(1)?.unwrap_or_default();
        self.accounts
            .ensure(
                &pubkeys,
                AccountFetchEntrypoint::RpcGetMultipleAccounts,
                claims,
            )
            .await;
        let block = self.engine.blocks().latest();
        let encoder = AccountEncoder::from(config);
        let accounts = self.engine.accounts();
        let loader = accounts.loader();
        let accounts = pubkeys
            .iter()
            .map(|pubkey| encoder.read_visible(&loader, pubkey, block.time))
            .collect::<Vec<_>>();
        ResponsePayload::encode(&request.id, accounts, block.slot)
    }
}
