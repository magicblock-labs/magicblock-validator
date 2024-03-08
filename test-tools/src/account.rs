use std::str::FromStr;

use sleipnir_bank::bank::Bank;
use solana_sdk::{
    account::{Account, AccountSharedData},
    clock::Epoch,
    pubkey::Pubkey,
    system_program,
};

pub fn fund_account(bank: &Bank, pubkey: &Pubkey, lamports: u64) {
    bank.store_account(
        pubkey,
        &Account {
            lamports,
            data: vec![],
            owner: system_program::id(),
            executable: false,
            rent_epoch: Epoch::MAX,
        },
    );
}

pub fn fund_account_addr(bank: &Bank, addr: &str, lamports: u64) {
    fund_account(bank, &Pubkey::from_str(addr).unwrap(), lamports);
}

pub fn get_account_addr(bank: &Bank, addr: &str) -> Option<AccountSharedData> {
    let pubkey = Pubkey::from_str(addr).unwrap();
    bank.get_account(&pubkey)
}
