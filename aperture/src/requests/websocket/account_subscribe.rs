use solana_account::AccountSharedData;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;
use crate::{account::encoding::AccountEncoder, requests::payload::NotificationPayload};

impl WsDispatcher {
    pub(crate) async fn account_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let pubkey = request.required::<Serde32Bytes>(0)?.into();
        let config = request.optional::<RpcAccountInfoConfig>(1)?.unwrap_or_default();
        let encoder = AccountEncoder::from(config);

        let rx = self.engine.accounts().subscribe(pubkey).await;
        let engine = self.engine.clone();
        Ok(self.forward(rx, move |id, account: AccountSharedData| {
            let block = engine.blocks().latest();
            let account = encoder.encode(&pubkey, &account, |mint| {
                mint.read(&engine.accounts().loader(), block.time)
            });
            NotificationPayload::encode(account, block.slot, "accountNotification", id).map(Some)
        }))
    }
}
