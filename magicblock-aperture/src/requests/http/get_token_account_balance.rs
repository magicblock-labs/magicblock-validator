use magicblock_metrics::metrics::AccountFetchEntrypoint;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_account_decoder::{
    parse_account_data::SplTokenAdditionalDataV2,
    parse_token::{is_known_spl_token_id, token_amount_to_ui_amount_v3},
};
use solana_pubkey::Pubkey;
use spl_token_2022::{
    extension::StateWithExtensions,
    state::{Account as TokenAccount, AccountState, Mint},
};

use super::ClaimedHandlerResult;
use crate::{
    error::RpcError,
    requests::{
        JsonHttpRequest as JsonRequest, params::Serde32Bytes,
        payload::ResponsePayload,
    },
    server::http::dispatch::HttpDispatcher,
};

impl HttpDispatcher {
    pub(crate) async fn get_token_account_balance(
        &self,
        request: &JsonRequest,
    ) -> ClaimedHandlerResult {
        let mut claims = 0;
        let result = async {
            let pubkey: Pubkey = request.required::<Serde32Bytes>(0)?.into();
            let reader = |account: &AccountSharedData| {
                if !is_known_spl_token_id(account.owner()) {
                    return Err(RpcError::invalid_params(
                        "account is not a token account",
                    ));
                }
                let token =
                    StateWithExtensions::<TokenAccount>::unpack(account.data())
                        .map_err(|_| {
                            RpcError::invalid_params(
                                "invalid token account data",
                            )
                        })?;
                if token.base.state == AccountState::Uninitialized {
                    return Err(RpcError::invalid_params(
                        "token account is not initialized",
                    ));
                }
                Ok((*account.owner(), token.base.mint, token.base.amount))
            };

            let (token_account, remote_account_claims) = self
                .read_account_with_ensure(
                    &pubkey,
                    AccountFetchEntrypoint::RpcGetAccount,
                    reader,
                )
                .await;
            claims += remote_account_claims;
            let (token_owner, mint, amount) =
                token_account.ok_or_else(|| {
                    RpcError::invalid_params("token account not found")
                })??;
            let reader = |account: &AccountSharedData| {
                if account.owner() != &token_owner {
                    return Err(RpcError::invalid_params("invalid mint owner"));
                }
                let mint = StateWithExtensions::<Mint>::unpack(account.data())
                    .map_err(|_| {
                        RpcError::invalid_params("invalid mint account data")
                    })?;
                if !mint.base.is_initialized {
                    return Err(RpcError::invalid_params(
                        "mint is not initialized",
                    ));
                }
                Ok(mint.base.decimals)
            };

            let (mint_account, remote_account_claims) = self
                .read_account_with_ensure(
                    &mint,
                    AccountFetchEntrypoint::RpcGetAccount,
                    reader,
                )
                .await;
            claims += remote_account_claims;
            let decimals = mint_account.ok_or_else(|| {
                RpcError::invalid_params("mint account not found")
            })??;

            let ui_token_amount = token_amount_to_ui_amount_v3(
                amount,
                &SplTokenAdditionalDataV2 {
                    decimals,
                    interest_bearing_config: None,
                    scaled_ui_amount_config: None,
                },
            );

            let slot = self.engine.blocks().latest().slot;
            Ok(ResponsePayload::encode(&request.id, ui_token_amount, slot))
        }
        .await;
        (result, claims)
    }
}
