use solana_rpc_client_api::config::RpcProgramAccountsConfig;

use crate::utils::ProgramFilters;

use super::prelude::*;

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

        // Fetch all accounts owned by the program, applying
        // filters at the database level for efficiency.
        let accounts =
            self.accountsdb.get_program_accounts(&program, move |a| {
                filters.matches(a.data())
            })?;

        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);
        let slice = config.account_config.data_slice;

        // Encode the filtered accounts for the RPC response.
        let accounts = accounts
            .map(|(pubkey, account)| {
                // lock account to prevent data races with concurrently modifiying
                // transaction executor threads (unlikely, but not impossible)
                let locked = LockedAccount::new(pubkey, account);
                AccountWithPubkey::new(&locked, encoding, slice)
            })
            .collect::<Vec<_>>();

        if config.with_context.unwrap_or_default() {
            let slot = self.blocks.block_height();
            Ok(ResponsePayload::encode(&request.id, accounts, slot))
        } else {
            Ok(ResponsePayload::encode_no_context(&request.id, accounts))
        }
    }
}
