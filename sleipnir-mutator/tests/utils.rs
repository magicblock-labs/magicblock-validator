use solana_sdk::{pubkey, pubkey::Pubkey};
use test_tools::{account::fund_account, traits::TransactionsProcessor};

pub const SOLX_PROG: Pubkey =
    pubkey!("SoLXmnP9JvL6vJ7TN1VqtTxqsc2izmPfF9CsMDEuRzJ");
#[allow(dead_code)] // used in tests
pub const SOLX_EXEC: Pubkey =
    pubkey!("J1ct2BY6srXCDMngz5JxkX3sHLwCqGPhy9FiJBc8nuwk");
#[allow(dead_code)] // used in tests
pub const SOLX_IDL: Pubkey =
    pubkey!("EgrsyMAsGYMKjcnTvnzmpJtq3hpmXznKQXk21154TsaS");
#[allow(dead_code)] // used in tests
pub const SOLX_TIPS: Pubkey =
    pubkey!("SoLXtipsYqzgFguFCX6vw3JCtMChxmMacWdTpz2noRX");
#[allow(dead_code)] // used in tests
pub const SOLX_POST: Pubkey =
    pubkey!("5eYk1TwtEwsUTqF9FHhm6tdmvu45csFkKbC4W217TAts");

const LUZIFER: Pubkey = pubkey!("LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk");

pub fn fund_luzifer(bank: &dyn TransactionsProcessor) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account(bank.bank(), &LUZIFER, u64::MAX / 2);
}
