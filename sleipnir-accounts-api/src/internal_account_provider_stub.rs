use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use solana_sdk::{account::AccountSharedData, clock::Slot, pubkey::Pubkey};

use crate::InternalAccountProvider;

#[derive(Debug, Clone, Default)]
pub struct InternalAccountProviderStub {
    slot: Slot,
    accounts: Arc<RwLock<HashMap<Pubkey, AccountSharedData>>>,
}

impl InternalAccountProviderStub {
    pub fn add(&self, pubkey: Pubkey, account: AccountSharedData) {
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
    fn get_slot(&self) -> Slot {
        self.slot
    }
}
