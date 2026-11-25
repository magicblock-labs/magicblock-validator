use solana_account::{AccountSharedData, ReadableAccount};
use solana_account_decoder::parse_token::is_known_spl_token_id;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::{RpcAccountInfoConfig, RpcTokenAccountsFilter};
use spl_token_2022::{
    extension::StateWithExtensions,
    state::{Account as TokenAccount, Mint},
};

use super::{HandlerResult, RpcHandlers};
use crate::{
    account::encoding::AccountEncoder,
    error::RpcError,
    requests::{JsonHttpRequest as JsonRequest, params::Serde32Bytes, payload::ResponsePayload},
};

#[derive(Clone, Copy)]
pub(crate) enum TokenAccountAuthority {
    Owner,
    Delegate,
}

impl RpcHandlers {
    pub(crate) fn get_token_accounts(
        &self,
        request: &JsonRequest,
        authority: TokenAccountAuthority,
    ) -> HandlerResult {
        let authority_key: Pubkey = request.required::<Serde32Bytes>(0)?.into();
        let filter = request.required::<RpcTokenAccountsFilter>(1)?;
        let config = request.optional::<RpcAccountInfoConfig>(2)?.unwrap_or_default();

        let (program, mint) = match filter {
            RpcTokenAccountsFilter::Mint(mint) => {
                let mint: Pubkey = mint.parse().map_err(RpcError::invalid_params)?;
                let reader = |account: &AccountSharedData| {
                    (
                        *account.owner(),
                        is_known_spl_token_id(account.owner())
                            && StateWithExtensions::<Mint>::unpack(account.data()).is_ok(),
                    )
                };
                let (program, valid) = self
                    .engine
                    .accounts()
                    .loader()
                    .read(&mint, reader)
                    .map_err(RpcError::internal)?
                    .ok_or_else(|| RpcError::invalid_params("mint account not found"))?;
                if !valid {
                    return Err(RpcError::invalid_params("invalid mint account"));
                }
                (program, Some(mint))
            }
            RpcTokenAccountsFilter::ProgramId(program) => {
                let program: Pubkey = program.parse().map_err(RpcError::invalid_params)?;
                if !is_known_spl_token_id(&program) {
                    return Err(RpcError::invalid_params("unknown token program id"));
                }
                (program, None)
            }
        };

        let encoder = AccountEncoder::from(config);
        let accounts = encoder.scan_program(&self.engine, &program, |account| {
            let Ok(token) = StateWithExtensions::<TokenAccount>::unpack(account.data()) else {
                return false;
            };
            let token = token.base;
            if mint.is_some_and(|mint| token.mint != mint) {
                return false;
            }
            match authority {
                TokenAccountAuthority::Owner => token.owner == authority_key,
                TokenAccountAuthority::Delegate => token.delegate.contains(&authority_key),
            }
        })?;

        let slot = self.engine.blocks().latest().slot;
        ResponsePayload::encode(&request.id, accounts, slot)
    }
}
