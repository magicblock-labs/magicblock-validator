use solana_account::ReadableAccount;
use solana_account_decoder::{
    UiAccountEncoding, parse_token::is_known_spl_token_id,
};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::{
    RpcAccountInfoConfig, RpcTokenAccountsFilter,
};
use spl_token_2022::{
    extension::StateWithExtensions,
    state::{Account as TokenAccount, AccountState, Mint},
};

use super::{HandlerResult, get_program_accounts::AccountWithPubkey};
use crate::{
    error::RpcError,
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

#[derive(Clone, Copy)]
pub(crate) enum TokenAccountAuthority {
    Owner,
    Delegate,
}

impl HttpDispatcher {
    pub(crate) fn get_token_accounts(
        &self,
        request: &JsonRequest,
        authority: TokenAccountAuthority,
    ) -> HandlerResult {
        let authority_key: Pubkey = request.required::<Serde32Bytes>(0)?.into();
        let filter = request.required::<RpcTokenAccountsFilter>(1)?;
        let config = request
            .optional::<RpcAccountInfoConfig>(2)?
            .unwrap_or_default();

        let (program, mint) = match filter {
            RpcTokenAccountsFilter::Mint(mint) => {
                let mint: Pubkey =
                    mint.parse().map_err(RpcError::invalid_params)?;
                let account = self
                    .engine
                    .accounts()
                    .get(&mint)
                    .map_err(RpcError::internal)?
                    .ok_or_else(|| {
                        RpcError::invalid_params("mint account not found")
                    })?;
                if !is_known_spl_token_id(account.owner())
                    || StateWithExtensions::<Mint>::unpack(account.data())
                        .is_err()
                {
                    return Err(RpcError::invalid_params(
                        "invalid mint account",
                    ));
                }
                (*account.owner(), Some(mint))
            }
            RpcTokenAccountsFilter::ProgramId(program) => {
                let program: Pubkey =
                    program.parse().map_err(RpcError::invalid_params)?;
                if !is_known_spl_token_id(&program) {
                    return Err(RpcError::invalid_params(
                        "unknown token program id",
                    ));
                }
                (program, None)
            }
        };

        let encoding = config.encoding.unwrap_or(UiAccountEncoding::Base58);
        let slice = config.data_slice;
        let accounts = self
            .engine
            .accounts()
            .program(&program)
            .map_err(RpcError::internal)?
            .filter_map(|(pubkey, account)| {
                let token =
                    StateWithExtensions::<TokenAccount>::unpack(account.data())
                        .ok()?;
                if token.base.state == AccountState::Uninitialized
                    || mint.is_some_and(|mint| token.base.mint != mint)
                {
                    return None;
                }
                let matches = match authority {
                    TokenAccountAuthority::Owner => {
                        token.base.owner == authority_key
                    }
                    TokenAccountAuthority::Delegate => {
                        token.base.delegate.contains(&authority_key)
                    }
                };
                matches.then(|| {
                    AccountWithPubkey::new(pubkey, &account, encoding, slice)
                })
            })
            .collect::<Vec<_>>();

        let slot = self.engine.blocks().latest().slot;
        Ok(ResponsePayload::encode(&request.id, accounts, slot))
    }
}
