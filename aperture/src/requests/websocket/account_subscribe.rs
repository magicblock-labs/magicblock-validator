use solana_account::AccountSharedData;
use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use super::prelude::*;
use crate::encoder::AccountEncoder;

impl WsDispatcher {
    pub(crate) fn account_subscribe(&mut self, request: &JsonRequest) -> RpcResult<SubResult> {
        let pubkey = request.required::<Serde32Bytes>(0)?.into();
        let config = request.optional::<RpcAccountInfoConfig>(1)?.unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let encoder = AccountEncoder {
            encoding,
            data_slice: config.data_slice,
        };

        let id = next_subid();
        let rx = self.engine.accounts().subscribe(pubkey);
        let engine = self.engine.clone();
        self.forward(id, rx, move |account: AccountSharedData| {
            encoder.encode(context_slot(&engine), &pubkey, &account, id).map(Some)
        });

        Ok(SubResult::SubId(id))
    }
}
