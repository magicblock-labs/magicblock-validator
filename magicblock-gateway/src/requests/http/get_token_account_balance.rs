use hyper::Response;
use magicblock_gateway_types::accounts::{Pubkey, ReadableAccount};
use solana_account_decoder::parse_token::UiTokenAmount;

use crate::{
    error::RpcError,
    requests::{
        http::{SPL_DECIMALS_OFFSET, SPL_MINT_RANGE, SPL_TOKEN_AMOUNT_RANGE},
        params::Serde32Bytes,
        payload::ResponsePayload,
        JsonRequest,
    },
    server::http::dispatch::HttpDispatcher,
    unwrap,
    utils::JsonBody,
};

impl HttpDispatcher {
    pub(crate) async fn get_token_account_balance(
        &self,
        request: JsonRequest,
    ) -> Response<JsonBody> {
        let params = request
            .params
            .ok_or_else(|| RpcError::invalid_request("missing params"));
        unwrap!(mut params, &request.id);
        let pubkey = parse_params!(params, Serde32Bytes);
        let pubkey = pubkey.map(Into::into).ok_or_else(|| {
            RpcError::invalid_params("missing or invalid pubkey")
        });
        unwrap!(pubkey, &request.id);
        let token_account =
            self.read_account_with_ensure(&pubkey).await.ok_or_else(|| {
                RpcError::invalid_params("token account is not found")
            });
        unwrap!(token_account, request.id);
        let mint = token_account
            .data()
            .get(SPL_MINT_RANGE)
            .map(Pubkey::try_from)
            .transpose()
            .map_err(|e| RpcError::invalid_params(e));
        unwrap!(mint, request.id);
        let mint = mint
            .ok_or_else(|| RpcError::invalid_params("invalid token account"));
        unwrap!(mint, request.id);
        let mint_account =
            self.read_account_with_ensure(&mint).await.ok_or_else(|| {
                RpcError::invalid_params("mint account doesn't exist")
            });
        unwrap!(mint_account, request.id);
        let decimals = mint_account
            .data()
            .get(SPL_DECIMALS_OFFSET)
            .copied()
            .ok_or_else(|| {
                RpcError::invalid_params("invalid token mint account")
            });
        unwrap!(decimals, request.id);
        let token_amount = {
            let slice =
                token_account.data().get(SPL_TOKEN_AMOUNT_RANGE).ok_or_else(
                    || RpcError::invalid_params("invalid token account"),
                );
            unwrap!(slice, request.id);
            let mut buffer = [0; size_of::<u64>()];
            buffer.copy_from_slice(slice);
            u64::from_le_bytes(buffer)
        };

        let ui_amount = (token_amount as f64) / 10f64.powf(decimals as f64);
        let ui_token_amount = UiTokenAmount {
            amount: token_amount.to_string(),
            ui_amount: Some(ui_amount),
            ui_amount_string: ui_amount.to_string(),
            decimals,
        };
        let slot = self.accountsdb.slot();
        ResponsePayload::encode(&request.id, ui_token_amount, slot)
    }
}
