use solana_rpc_client_api::config::RpcProgramAccountsConfig;

use crate::utils::ProgramFilters;

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_program_accounts(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (program, config) = parse_params!(
            request.params()?,
            Serde32Bytes,
            RpcProgramAccountsConfig
        );
        let program = some_or_err!(program);
        let config = config.unwrap_or_default();
        let filters = ProgramFilters::from(config.filters);
        let accounts =
            self.accountsdb.get_program_accounts(&program, move |a| {
                filters.matches(a.data())
            })?;
        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);
        let slice = config.account_config.data_slice;
        let accounts = accounts
            .map(|(pubkey, account)| {
                let locked = LockedAccount::new(pubkey, account);
                AccountWithPubkey::new(&locked, encoding, slice)
            })
            .collect::<Vec<_>>();
        if config.with_context.unwrap_or_default() {
            let slot = self.accountsdb.slot();
            Ok(ResponsePayload::encode(&request.id, accounts, slot))
        } else {
            Ok(ResponsePayload::encode_no_context(&request.id, accounts))
        }
    }
}
