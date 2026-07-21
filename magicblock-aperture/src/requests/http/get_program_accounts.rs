use solana_rpc_client_api::config::RpcProgramAccountsConfig;

use super::prelude::*;
use crate::utils::ProgramFilters;

impl HttpDispatcher {
    /// Handles the `getProgramAccounts` RPC request.
    ///
    /// Fetches all accounts owned by a given program public key. The request can be
    /// customized with an optional configuration object to apply server-side data
    /// filters, specify the data encoding, request a slice of the account data,
    /// and control whether the result is wrapped in a context object.
    pub(crate) fn get_program_accounts(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (program_bytes, config) = parse_params!(
            request.params()?,
            Serde32Bytes,
            RpcProgramAccountsConfig
        );
        let program: Pubkey = some_or_err!(program_bytes);
        let config = config.unwrap_or_default();
        let filters = ProgramFilters::from(config.filters);

        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);
        let slice = config.account_config.data_slice;

        // Scan all accounts owned by the program through the engine, applying
        // the server-side filters and encoding for the RPC response.
        let accounts = self.engine.accounts();
        let accounts = accounts
            .program(&program)
            .map_err(RpcError::internal)?
            .filter(|(_, account)| filters.matches(account.data()))
            .map(|(pubkey, account)| {
                AccountWithPubkey::new(pubkey, &account, encoding, slice)
            })
            .collect::<Vec<_>>();

        if config.with_context.unwrap_or_default() {
            let slot = self.engine.blocks().latest().slot;
            Ok(ResponsePayload::encode(&request.id, accounts, slot))
        } else {
            Ok(ResponsePayload::encode_no_context(&request.id, accounts))
        }
    }
}
