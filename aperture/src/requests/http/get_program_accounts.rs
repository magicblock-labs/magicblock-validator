use super::{HandlerResult, RpcHandlers};
use crate::{
    account::{ProgramConfig, encoding::AccountEncoder},
    requests::{JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload},
};
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;

impl RpcHandlers {
    pub(crate) fn get_program_accounts(&self, request: &JsonRequest) -> HandlerResult {
        let program: Pubkey = request.required::<Serde32Bytes>(0)?.into();
        let ProgramConfig { config, filters } =
            request.optional::<ProgramConfig>(1)?.unwrap_or_default();
        let filters = filters.unwrap_or_default();

        let encoder = AccountEncoder::from(config.account_config);
        let accounts = encoder.scan_program(&self.engine, &program, |account| {
            filters.iter().all(|filter| filter.matches(account.data()))
        })?;

        if config.with_context.unwrap_or_default() {
            let slot = self.engine.blocks().latest().slot;
            ResponsePayload::encode(&request.id, accounts, slot)
        } else {
            ResponsePayload::encode_no_context(&request.id, accounts)
        }
    }
}
