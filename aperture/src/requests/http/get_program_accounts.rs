use super::{HandlerResult, RpcHandlers};
use crate::account::{AccountWithPubkey, ProgramConfig, matches_filters};
use crate::{
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload},
};
use solana_account::ReadableAccount;
use solana_account_decoder::UiAccountEncoding;
use solana_pubkey::Pubkey;

impl RpcHandlers {
    pub(crate) fn get_program_accounts(&self, request: &JsonRequest) -> HandlerResult {
        let program: Pubkey = request.required::<Serde32Bytes>(0)?.into();
        let ProgramConfig { config, filters } =
            request.optional::<ProgramConfig>(1)?.unwrap_or_default();
        let filters = filters.unwrap_or_default();

        let encoding = config.account_config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.account_config.data_slice;

        let accounts = self.engine.accounts();
        let accounts = accounts
            .program(&program, |pubkey, account| {
                matches_filters(&filters, account.data())
                    .then(|| AccountWithPubkey::new(*pubkey, account, encoding, slice))
            })
            .map_err(RpcError::internal)?
            .filter_map(|(_, account)| account)
            .collect::<Vec<_>>();

        if config.with_context.unwrap_or_default() {
            let slot = self.engine.blocks().latest().slot;
            ResponsePayload::encode(&request.id, accounts, slot)
        } else {
            ResponsePayload::encode_no_context(&request.id, accounts)
        }
    }
}
