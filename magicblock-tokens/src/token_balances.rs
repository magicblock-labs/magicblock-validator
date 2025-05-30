// NOTE: from ledger/src/token_balances.rs with only imports adjusted
use std::collections::HashMap;

use magicblock_bank::{bank::Bank, transaction_batch::TransactionBatch};
use magicblock_transaction_status::{
    token_balances::TransactionTokenBalances, TransactionTokenBalance,
};
use solana_account_decoder::{
    parse_account_data::SplTokenAdditionalDataV2,
    parse_token::{
        is_known_spl_token_id, token_amount_to_ui_amount_v3, UiTokenAmount,
    },
};
use solana_measure::measure::Measure;
use solana_metrics::datapoint_debug;
use solana_sdk::{
    account::ReadableAccount, program_error::ProgramError, pubkey::Pubkey,
};
use spl_token_2022::{
    extension::StateWithExtensions,
    state::{Account as TokenAccount, Mint},
};

pub fn get_mint_decimals(bank: &Bank, mint: &Pubkey) -> Option<u8> {
    if mint == &spl_token::native_mint::id() {
        Some(spl_token::native_mint::DECIMALS)
    } else {
        let mint_account = bank.get_account(mint)?;

        if !is_known_spl_token_id(mint_account.owner()) {
            return None;
        }

        get_mint_decimals_from_data(mint_account.data()).ok()
    }
}

pub fn get_mint_decimals_from_data(data: &[u8]) -> Result<u8, ProgramError> {
    StateWithExtensions::<Mint>::unpack(data).map(|mint| mint.base.decimals)
}

pub fn collect_token_balances(
    bank: &Bank,
    batch: &TransactionBatch,
    mint_decimals: &mut HashMap<Pubkey, u8>,
) -> TransactionTokenBalances {
    let mut balances: TransactionTokenBalances = vec![];
    let mut collect_time = Measure::start("collect_token_balances");

    for transaction in batch.sanitized_transactions() {
        let account_keys = transaction.message().account_keys();
        let has_token_program = account_keys.iter().any(is_known_spl_token_id);

        let mut transaction_balances: Vec<TransactionTokenBalance> = vec![];
        if has_token_program {
            for (index, account_id) in account_keys.iter().enumerate() {
                if transaction.message().is_invoked(index)
                    || is_known_spl_token_id(account_id)
                {
                    continue;
                }

                if let Some(TokenBalanceData {
                    mint,
                    ui_token_amount,
                    owner,
                    program_id,
                }) = collect_token_balance_from_account(
                    bank,
                    account_id,
                    mint_decimals,
                ) {
                    transaction_balances.push(TransactionTokenBalance {
                        account_index: index as u8,
                        mint,
                        ui_token_amount,
                        owner,
                        program_id,
                    });
                }
            }
        }
        balances.push(transaction_balances);
    }
    collect_time.stop();
    datapoint_debug!(
        "collect_token_balances",
        ("collect_time_us", collect_time.as_us(), i64),
    );
    balances
}

#[derive(Debug, PartialEq)]
struct TokenBalanceData {
    mint: String,
    owner: String,
    ui_token_amount: UiTokenAmount,
    program_id: String,
}

fn collect_token_balance_from_account(
    bank: &Bank,
    account_id: &Pubkey,
    mint_decimals: &mut HashMap<Pubkey, u8>,
) -> Option<TokenBalanceData> {
    let account = bank.get_account(account_id)?;

    if !is_known_spl_token_id(account.owner()) {
        return None;
    }

    let token_account =
        StateWithExtensions::<TokenAccount>::unpack(account.data()).ok()?;
    let mint = token_account.base.mint;

    let decimals = mint_decimals.get(&mint).cloned().or_else(|| {
        let decimals = get_mint_decimals(bank, &mint)?;
        mint_decimals.insert(mint, decimals);
        Some(decimals)
    })?;

    Some(TokenBalanceData {
        mint: token_account.base.mint.to_string(),
        owner: token_account.base.owner.to_string(),
        ui_token_amount: token_amount_to_ui_amount_v3(
            token_account.base.amount,
            &SplTokenAdditionalDataV2::with_decimals(decimals),
        ),
        program_id: account.owner().to_string(),
    })
}
