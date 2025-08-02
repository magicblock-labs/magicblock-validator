use std::convert::identity;

use hyper::Response;
use magicblock_gateway_types::accounts::{
    AccountsToEnsure, LockedAccount, Pubkey,
};
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use crate::{
    error::RpcError,
    requests::{params::Serde32Bytes, payload::ResponsePayload, JsonRequest},
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) async fn get_multiple_accounts(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, request.id);
        let (pubkeys, config) =
            parse_params!(params, Vec<Serde32Bytes>, RpcAccountInfoConfig);
        let pubkeys =
            pubkeys.ok_or_else(|| RpcError::invalid_params("missing pubkeys"));
        unwrap!(pubkeys, request.id);
        // SAFETY: Pubkey has the same memory layout and size as Serde32Bytes
        let pubkeys: Vec<Pubkey> = unsafe { std::mem::transmute(pubkeys) };
        let config = config.unwrap_or_default();
        let slot = self.accountsdb.slot();
        let mut accounts = vec![None; pubkeys.len()];
        let mut ensured = false;
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        loop {
            let reader = self.accountsdb.reader().map_err(RpcError::internal);
            unwrap!(reader, request.id);
            for (pubkey, account) in pubkeys.iter().zip(&mut accounts) {
                if account.is_some() {
                    continue;
                }
                *account = reader.read(pubkey, identity).map(|acc| {
                    LockedAccount::new(*pubkey, acc).ui_encode(encoding)
                });
            }
            if ensured {
                break;
            }
            let to_ensure = accounts
                .iter()
                .zip(&pubkeys)
                .filter_map(|(acc, pk)| acc.is_none().then_some(*pk))
                .collect::<Vec<_>>();
            if to_ensure.is_empty() {
                break;
            }
            let to_ensure = AccountsToEnsure::new(to_ensure);
            let ready = to_ensure.ready.clone();
            let _check = self
                .ensure_accounts_tx
                .send(to_ensure)
                .await
                .map_err(RpcError::internal);
            unwrap!(_check, request.id);
            ready.notified().await;
            ensured = true;
        }

        ResponsePayload::encode(&request.id, accounts, slot)
    }
}
