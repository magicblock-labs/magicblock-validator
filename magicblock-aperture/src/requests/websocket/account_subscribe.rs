use solana_account::AccountSharedData;
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;
use crate::encoder::AccountEncoder;

impl WsDispatcher {
    pub(crate) async fn account_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let pubkey = request.required::<Serde32Bytes>(0)?.into();
        let config = request
            .optional::<RpcAccountInfoConfig>(1)?
            .unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let encoder = AccountEncoder {
            encoding,
            data_slice: config.data_slice,
        };

        let id = next_subid();
        let mut rx = self.engine.accounts().subscribe(pubkey).await;
        let tx = self.chan.tx.clone();
        let engine = self.engine.clone();
        let handle = tokio::spawn(async move {
            while let Some(account) = rx.recv().await {
                let account: AccountSharedData = account;
                let slot = context_slot(&engine);
                let Some(bytes) = encoder.encode(slot, &pubkey, &account, id)
                else {
                    continue;
                };
                if tx.send(bytes).await.is_err() {
                    break;
                }
            }
        });
        self.register(id, handle);

        Ok(SubResult::SubId(id))
    }
}
