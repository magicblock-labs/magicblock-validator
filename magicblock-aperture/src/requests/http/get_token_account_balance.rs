use std::mem::size_of;

use solana_account::AccountSharedData;
use solana_account_decoder::parse_token::UiTokenAmount;

use super::{
    prelude::*, MINT_DECIMALS_OFFSET, SPL_MINT_RANGE, SPL_TOKEN_AMOUNT_RANGE,
};

impl HttpDispatcher {
    /// Handles the `getTokenAccountBalance` RPC request.
    ///
    /// Returns the token balance of a given SPL Token account, formatted as a
    /// `UiTokenAmount`. This involves a two-step process: first fetching the
    /// token account to identify its mint, and then fetching the mint account
    /// to determine the token's decimal precision for the UI representation.
    pub(crate) async fn get_token_account_balance(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let pubkey = parse_params!(request.params()?, Serde32Bytes);
        let pubkey: Pubkey = some_or_err!(pubkey);

        // Fetch the target token account.
        let token_account: AccountSharedData = some_or_err!(
            self.read_account_with_ensure(&pubkey).await,
            "token account not found or is not a token account"
        );

        // Parse the mint address from the token account's data.
        let mint_pubkey: Pubkey = token_account
            .data()
            .get(SPL_MINT_RANGE)
            .and_then(|slice| slice.try_into().ok())
            .map(Pubkey::new_from_array)
            .ok_or_else(|| {
                RpcError::invalid_params("invalid token account data")
            })?;

        // Fetch the mint account to get the token's decimals.
        let mint_account: AccountSharedData = some_or_err!(
            self.read_account_with_ensure(&mint_pubkey).await,
            "mint account not found"
        );
        let decimals =
            *mint_account.data().get(MINT_DECIMALS_OFFSET).ok_or_else(
                || RpcError::invalid_params("invalid mint account data"),
            )?;

        // Parse the raw token amount from the token account's data.
        let token_amount = {
            let slice = some_or_err!(
                token_account.data().get(SPL_TOKEN_AMOUNT_RANGE),
                "invalid token account data"
            );
            let mut buffer = [0; size_of::<u64>()];
            buffer.copy_from_slice(slice);
            u64::from_le_bytes(buffer)
        };

        let ui_amount = (token_amount as f64) / 10f64.powi(decimals as i32);
        let ui_token_amount = UiTokenAmount {
            amount: token_amount.to_string(),
            ui_amount: Some(ui_amount),
            ui_amount_string: ui_amount.to_string(),
            decimals,
        };

        let slot = self.blocks.block_height();
        Ok(ResponsePayload::encode(&request.id, ui_token_amount, slot))
    }
}
