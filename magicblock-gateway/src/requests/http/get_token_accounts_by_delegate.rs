use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcTokenAccountsFilter,
};

use crate::{
    requests::http::{SPL_DELEGATE_OFFSET, SPL_MINT_OFFSET, TOKEN_PROGRAM_ID},
    utils::{ProgramFilter, ProgramFilters},
};

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_token_accounts_by_delegate(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let (delegate, filter, config) = parse_params!(
            request.params()?,
            Serde32Bytes,
            RpcTokenAccountsFilter,
            RpcAccountInfoConfig
        );
        let delegate: Serde32Bytes = some_or_err!(delegate);
        let filter = some_or_err!(filter);
        let config = config.unwrap_or_default();
        let slot = self.accountsdb.slot();
        let mut filters = ProgramFilters::default();
        let mut program = TOKEN_PROGRAM_ID;
        match filter {
            RpcTokenAccountsFilter::Mint(pubkey) => {
                let bytes = bs58::decode(pubkey)
                    .into_vec()
                    .map_err(RpcError::parse_error)?;
                let filter = ProgramFilter::MemCmp {
                    offset: SPL_MINT_OFFSET,
                    bytes,
                };
                filters.push(filter);
            }
            RpcTokenAccountsFilter::ProgramId(pubkey) => {
                program = pubkey.parse().map_err(RpcError::parse_error)?
            }
        };
        filters.push(ProgramFilter::MemCmp {
            offset: SPL_DELEGATE_OFFSET,
            bytes: delegate.0.to_vec(),
        });
        let accounts =
            self.accountsdb.get_program_accounts(&program, move |a| {
                filters.matches(a.data())
            })?;
        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;
        let accounts = accounts
            .into_iter()
            .map(|(pubkey, account)| {
                let locked = LockedAccount::new(pubkey, account);
                AccountWithPubkey::new(&locked, encoding, slice)
            })
            .collect::<Vec<_>>();
        Ok(ResponsePayload::encode(&request.id, accounts, slot))
    }
}
