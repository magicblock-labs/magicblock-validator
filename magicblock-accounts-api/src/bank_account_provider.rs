use std::sync::Arc;

use magicblock_bank::bank::Bank;
use solana_sdk::{
    account::AccountSharedData, clock::Slot, hash::Hash, pubkey::Pubkey,
};

use crate::InternalAccountProvider;

pub struct AccountsDbProvider(Arc<AccountsDb>);

impl AccountsDbProvider {
    pub fn new(accountsdb: Arc<AccountsDb>) -> Self {
        Self(accountsdb)
    }
}

impl InternalAccountProvider for AccountsDbProvider {
    fn has_account(&self, pubkey: &Pubkey) -> bool {
        self.0.contains_account(pubkey)
    }

    fn remove_account(&self, pubkey: &Pubkey) {
        self.0.remove_account(pubkey);
    }

    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.0.get_account(pubkey)
    }
    fn get_all_accounts(&self) -> Vec<(Pubkey, AccountSharedData)> {
        self.0.iter_all().collect()
    }
    fn get_slot(&self) -> u64 {
        self.0.slot()
    }
    fn get_blockhash(&self) -> Hash {
        self.bank.last_blockhash()
    }
}
