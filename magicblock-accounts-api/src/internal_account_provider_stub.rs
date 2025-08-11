use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;

use crate::InternalAccountProvider;

#[derive(Debug, Clone, Default)]
pub struct InternalAccountProviderStub {
    slot: u64,
    accounts: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
}

impl InternalAccountProviderStub {
    pub fn set(&self, pubkey: Pubkey, account: AccountSharedData) {
        self.accounts.write().unwrap().insert(pubkey, account);
    }
}

impl InternalAccountProvider for InternalAccountProviderStub {
    fn has_account(&self, pubkey: &Pubkey) -> bool {
        self.accounts.read().unwrap().contains_key(pubkey)
    }
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.accounts.read().unwrap().get(pubkey).cloned()
    }
    fn get_all_accounts(&self) -> Vec<(Pubkey, AccountSharedData)> {
        self.accounts
            .read()
            .unwrap()
            .iter()
            .map(|(pubkey, account)| (*pubkey, account.clone()))
            .collect()
    }
    fn get_slot(&self) -> u64 {
        self.slot
    }
}
