use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{config::RpcAccountInfoConfig, request::MAX_MULTIPLE_ACCOUNTS};

use super::{HandlerResult, RpcHandlers};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload},
};

impl RpcHandlers {
    /// Same shape as `getMultipleAccounts`. A visible local image is returned
    /// as-is. Any other key is read from base without being cloned into
    /// accountsdb or subscribed.
    pub(crate) async fn get_multiple_peek_accounts(&self, request: &JsonRequest) -> HandlerResult {
        let pubkeys = request.required::<Vec<Serde32Bytes>>(0)?;
        if pubkeys.len() > MAX_MULTIPLE_ACCOUNTS {
            return Err(RpcError::invalid_params(format!(
                "Too many accounts provided; max {MAX_MULTIPLE_ACCOUNTS}"
            )));
        }
        let pubkeys: Vec<Pubkey> = pubkeys.into_iter().map(Into::into).collect();

        let config = request.optional::<RpcAccountInfoConfig>(1)?.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;

        let accounts = self
            .accounts
            .peek_many(&pubkeys, |pubkey, account| {
                encode_ui_account(pubkey, account, encoding, None, slice)
            })
            .await
            .map_err(RpcError::internal)?;

        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode(&request.id, accounts, slot)
    }
}
