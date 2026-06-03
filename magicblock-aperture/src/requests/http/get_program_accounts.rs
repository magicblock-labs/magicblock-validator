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
    pub(crate) async fn get_program_accounts(
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
        #[cfg_attr(not(feature = "query-filtering"), allow(unused_mut))]
        let mut accounts = accounts.collect::<Vec<_>>();

        #[cfg(feature = "query-filtering")]
        if let Some(user) = &request.authenticated_user {
            use magicblock_metrics::metrics::AccountFetchOrigin::*;
            let account_pubkeys = accounts
                .iter()
                .map(|(pubkey, _)| *pubkey)
                .collect::<Vec<_>>();
            self.ensure_permission_accounts(
                &account_pubkeys,
                GetMultipleAccounts,
            )
            .await?;
            let permissions =
                magicblock_query_filtering::permissions_for_accounts(
                    &*self.accountsdb,
                    &account_pubkeys,
                )
                .map_err(RpcError::internal)?;
            accounts = magicblock_query_filtering::filter_keyed_accounts(
                accounts,
                &permissions,
                user,
            );
        }

        let encoding = config
            .account_config
            .encoding
            .unwrap_or(UiAccountEncoding::Base58);
        let slice = config.account_config.data_slice;

        // Encode the filtered accounts for the RPC response.
        let accounts = accounts
            .into_iter()
            .map(|(pubkey, account)| {
                // lock account to prevent data races with concurrently modifying
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
