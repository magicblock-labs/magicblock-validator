use std::convert::identity;

use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn get_multiple_accounts(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (pubkeys, config) = parse_params!(
            request.params()?,
            Vec<Serde32Bytes>,
            RpcAccountInfoConfig
        );
        let pubkeys: Vec<_> = some_or_err!(pubkeys);
        // SAFETY: Pubkey has the same memory layout and size as Serde32Bytes
        let pubkeys: Vec<Pubkey> = unsafe { std::mem::transmute(pubkeys) };
        let config = config.unwrap_or_default();
        let slot = self.accountsdb.slot();
        let mut accounts = vec![None; pubkeys.len()];
        let mut ensured = false;
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        loop {
            let reader = self.accountsdb.reader()?;
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
            let _ = self.ensure_accounts_tx.send(to_ensure).await;
            ready.notified().await;
            ensured = true;
        }

        Ok(ResponsePayload::encode(&request.id, accounts, slot))
    }
}
