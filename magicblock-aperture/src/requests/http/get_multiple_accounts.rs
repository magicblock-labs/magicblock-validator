use magicblock_metrics::metrics::AccountFetchContext;
use solana_account::AccountSharedData;
use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::ClaimedHandlerResult;
use crate::{
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) async fn get_multiple_accounts(
        &self,
        request: &JsonRequest,
    ) -> ClaimedHandlerResult {
        let mut claims = 0;
        let result = async {
            let pubkeys = request.required::<Vec<Serde32Bytes>>(0)?;
            let pubkeys: Vec<Pubkey> =
                pubkeys.into_iter().map(Into::into).collect();

            let config = request
                .optional::<RpcAccountInfoConfig>(1)?
                .unwrap_or_default();
            let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
            let slice = config.data_slice;
            let reader = |pubkey: &Pubkey, account: &AccountSharedData| {
                HttpDispatcher::account_is_visible(account).then(|| {
                    encode_ui_account(pubkey, account, encoding, None, slice)
                })
            };

            let (ensured_accounts, remote_account_claims) = self
                .read_accounts_with_ensure(
                    &pubkeys,
                    AccountFetchContext::rpc_get_multiple_accounts(),
                    reader,
                )
                .await;
            claims += remote_account_claims;
            let accounts = ensured_accounts
                .into_iter()
                .map(Option::flatten)
                .collect::<Vec<_>>();

            let slot = self.engine.blocks().latest().slot;
            Ok(ResponsePayload::encode(&request.id, accounts, slot))
        }
        .await;
        (result, claims)
    }
}
