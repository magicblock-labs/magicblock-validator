use crate::account::ProgramConfig;
use solana_account_decoder::UiAccountEncoding;

use super::prelude::*;
use crate::encoder::{AccountEncoder, ProgramAccountEncoder};

impl WsDispatcher {
    pub(crate) async fn program_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let pubkey = request.required::<Serde32Bytes>(0)?.into();
        let ProgramConfig { config, filters } =
            request.optional::<ProgramConfig>(1)?.unwrap_or_default();

        let encoding = config.account_config.encoding.unwrap_or(UiAccountEncoding::Base58);

        let filters = filters.unwrap_or_default();
        let encoder = AccountEncoder {
            encoding,
            data_slice: config.account_config.data_slice,
        };
        let encoder = ProgramAccountEncoder { encoder, filters };

        let rx = self.engine.accounts().subscribe_program(pubkey).await;
        let engine = self.engine.clone();
        Ok(self.forward(rx, move |id, (pubkey, account)| {
            encoder.encode(context_slot(&engine), &pubkey, &account, id)
        }))
    }
}
