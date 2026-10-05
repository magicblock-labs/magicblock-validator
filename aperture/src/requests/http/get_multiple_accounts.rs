use crate::account::Accounts;
use magicblock_metrics::metrics::AccountFetchEntrypoint;
use solana_account::AccountSharedData;
use solana_account_decoder::{UiAccountEncoding, encode_ui_account};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{
    JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload,
};

impl RpcHandlers {
    pub(crate) async fn get_multiple_accounts(
        &self,
        request: &JsonRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        let pubkeys = request.required::<Vec<Serde32Bytes>>(0)?;
        let pubkeys: Vec<Pubkey> = pubkeys.into_iter().map(Into::into).collect();

        let config = request.optional::<RpcAccountInfoConfig>(1)?.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;
        let reader = |pubkey: &Pubkey, account: &AccountSharedData| {
            Accounts::account_is_visible(account)
                .then(|| encode_ui_account(pubkey, account, encoding, None, slice))
        };

        let ensured_accounts = self
            .accounts
            .read_many(
                &pubkeys,
                AccountFetchEntrypoint::RpcGetMultipleAccounts,
                claims,
                reader,
            )
            .await;
        let accounts = ensured_accounts.into_iter().map(Option::flatten).collect::<Vec<_>>();

        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode(&request.id, accounts, slot)
    }
}
