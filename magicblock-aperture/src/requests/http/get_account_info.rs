use magicblock_metrics::metrics::{AccountFetchContext, ENSURE_ACCOUNTS_TIME};
use solana_account::{AccountMode, AccountSharedData};
use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use tracing::warn;

use super::ClaimedHandlerResult;
use crate::{
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(super) fn account_is_visible(account: &AccountSharedData) -> bool {
        !account.is(AccountMode::Placeholder)
    }

    pub(super) async fn read_account_with_ensure<R>(
        &self,
        pubkey: &Pubkey,
        fetch_context: AccountFetchContext,
        reader: impl Fn(&AccountSharedData) -> R,
    ) -> (Option<R>, u64) {
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["account"])
            .start_timer();
        let claims = self
            .chainlink
            .ensure_accounts(&[*pubkey], fetch_context)
            .await
            .unwrap_or_default();
        let account = self
            .engine
            .accounts()
            .loader()
            .read(pubkey, reader)
            .ok()
            .flatten();
        (account, claims)
    }

    pub(super) async fn read_accounts_with_ensure<R>(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: AccountFetchContext,
        reader: impl Fn(&Pubkey, &AccountSharedData) -> R,
    ) -> (Vec<Option<R>>, u64) {
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["multi-account"])
            .start_timer();
        let claims = self
            .chainlink
            .ensure_accounts(pubkeys, fetch_context)
            .await
            .inspect_err(|error| warn!(?error, "failed to ensure accounts"))
            .unwrap_or_default();
        let accounts = {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            pubkeys
                .iter()
                .map(|pubkey| {
                    loader
                        .read(pubkey, |account| reader(pubkey, account))
                        .ok()
                        .flatten()
                })
                .collect()
        };
        (accounts, claims)
    }

    pub(crate) async fn get_account_info(
        &self,
        request: &JsonRequest,
    ) -> ClaimedHandlerResult {
        let mut claims = 0;
        let result = async {
            let pubkey: Pubkey = request.required::<Serde32Bytes>(0)?.into();
            let config = request
                .optional::<RpcAccountInfoConfig>(1)?
                .unwrap_or_default();
            let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
            let slice = config.data_slice;
            let reader = |account: &AccountSharedData| {
                Self::account_is_visible(account).then(|| {
                    encode_ui_account(&pubkey, account, encoding, None, slice)
                })
            };

            let (account, remote_account_claims) = self
                .read_account_with_ensure(
                    &pubkey,
                    AccountFetchContext::rpc_get_account(),
                    reader,
                )
                .await;
            claims += remote_account_claims;
            let account = account.flatten();

            let slot = self.engine.blocks().latest().slot;
            Ok(ResponsePayload::encode(&request.id, account, slot))
        }
        .await;
        (result, claims)
    }
}
