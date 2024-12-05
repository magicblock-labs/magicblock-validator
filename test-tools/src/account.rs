use magicblock_bank::bank::Bank;
use solana_sdk::{
    account::Account, clock::Epoch, pubkey::Pubkey, system_program,
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
