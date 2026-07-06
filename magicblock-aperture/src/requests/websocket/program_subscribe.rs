use solana_account_decoder::UiAccountEncoding;
use solana_rpc_client_api::config::RpcProgramAccountsConfig;

use super::prelude::*;
use crate::encoder::{AccountEncoder, ProgramAccountEncoder};

impl WsDispatcher {
    pub(crate) async fn program_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let pubkey = request.required::<Serde32Bytes>(0)?.into();
        let config = request
            .optional::<RpcProgramAccountsConfig>(1)?
            .unwrap_or_default();

        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);

        let filters = config.filters.unwrap_or_default();
        for filter in &filters {
            filter.verify().map_err(crate::RpcError::invalid_params)?;
        }
        let encoder = AccountEncoder {
            encoding,
            data_slice: config.account_config.data_slice,
        };
        let encoder = ProgramAccountEncoder { encoder, filters };

        let id = next_subid();
        let mut rx = self.engine.accounts().subscribe_program(pubkey).await;
        let tx = self.chan.tx.clone();
        let handle = tokio::spawn(async move {
            while let Ok((pubkey, account)) = rx.recv().await {
                let slot = account.slot();
                let Some(bytes) = encoder.encode(slot, &(pubkey, account), id)
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
