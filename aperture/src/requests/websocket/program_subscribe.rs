use solana_account::ReadableAccount;

use super::prelude::*;
use crate::{
    account::{ProgramConfig, encoding::AccountEncoder, matches_filters},
    requests::payload::NotificationPayload,
};

impl WsDispatcher {
    pub(crate) async fn program_subscribe(
        &mut self,
        request: &JsonRequest,
    ) -> RpcResult<SubResult> {
        let pubkey = request.required::<Serde32Bytes>(0)?.into();
        let ProgramConfig { config, filters } =
            request.optional::<ProgramConfig>(1)?.unwrap_or_default();

        let filters = filters.unwrap_or_default();
        let encoder = AccountEncoder::from(config.account_config);

        let rx = self.engine.accounts().subscribe_program(pubkey).await;
        let engine = self.engine.clone();
        Ok(self.forward(rx, move |id, (pubkey, account)| {
            if !matches_filters(&filters, account.data()) {
                return Ok(None);
            }
            let block = engine.blocks().latest();
            let encoded = encoder.encode(&pubkey, &account, |mint| {
                mint.read(&engine.accounts().loader(), block.time)
            });
            let Some(account) = encoder.program_row(account.owner(), pubkey, encoded) else {
                return Ok(None);
            };
            NotificationPayload::encode(account, block.slot, "programNotification", id).map(Some)
        }))
    }
}
