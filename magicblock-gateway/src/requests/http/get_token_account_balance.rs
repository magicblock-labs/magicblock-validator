use solana_account::AccountSharedData;
use solana_account_decoder::parse_token::UiTokenAmount;

use super::{SPL_DECIMALS_OFFSET, SPL_MINT_RANGE, SPL_TOKEN_AMOUNT_RANGE};

use super::prelude::*;

impl HttpDispatcher {
    pub(crate) async fn get_token_account_balance(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let pubkey = parse_params!(request.params()?, Serde32Bytes);
        let pubkey = some_or_err!(pubkey);
        let token_account: AccountSharedData = some_or_err!(
            self.read_account_with_ensure(&pubkey).await,
            "token account"
        );
        let mint = token_account
            .data()
            .get(SPL_MINT_RANGE)
            .map(Pubkey::try_from)
            .transpose()
            .map_err(RpcError::invalid_params)?;
        let mint = some_or_err!(mint);
        let mint_account: AccountSharedData = some_or_err!(
            self.read_account_with_ensure(&mint).await,
            "mint account"
        );
        let decimals = some_or_err!(
            mint_account.data().get(SPL_DECIMALS_OFFSET).copied(),
            "mint account"
        );
        let token_amount = {
            let slice = some_or_err!(
                token_account.data().get(SPL_TOKEN_AMOUNT_RANGE),
                "token account"
            );
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
        Ok(ResponsePayload::encode(&request.id, ui_token_amount, slot))
    }
}
