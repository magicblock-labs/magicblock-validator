use std::sync::Arc;

use magicblock_bank::bank::Bank;
use solana_sdk::{account::AccountSharedData, clock::Slot, pubkey::Pubkey};

use crate::InternalAccountProvider;

pub struct BankAccountProvider {
    bank: Arc<Bank>,
}

impl BankAccountProvider {
    pub fn new(bank: Arc<Bank>) -> Self {
        Self { bank }
    }
}

impl InternalAccountProvider for BankAccountProvider {
    fn has_account(&self, pubkey: &Pubkey) -> bool {
        self.bank.has_account(pubkey)
    }

    fn remove_account(&self, pubkey: &Pubkey) {
        self.bank.accounts_db.remove_account(pubkey);
    }

    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.bank.get_account(pubkey)
    }
    fn get_all_accounts(&self) -> Vec<(Pubkey, AccountSharedData)> {
        self.bank.get_all_accounts(false).collect()
    }
    fn get_slot(&self) -> Slot {
        self.bank.slot()
    }
}
