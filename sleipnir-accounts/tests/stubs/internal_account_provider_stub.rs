use std::collections::HashMap;

use sleipnir_accounts::InternalAccountProvider;
use solana_sdk::{account::AccountSharedData, pubkey::Pubkey};

#[derive(Default, Debug)]
pub struct InternalAccountProviderStub {
    accounts: HashMap<Pubkey, AccountSharedData>,
}

impl InternalAccountProviderStub {
    pub fn add(&mut self, pubkey: Pubkey, account: AccountSharedData) {
        self.accounts.insert(pubkey, account);
    }
}

impl InternalAccountProvider for InternalAccountProviderStub {
    fn has_account(&self, pubkey: &Pubkey) -> bool {
        self.accounts.contains_key(pubkey)
    }
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.accounts.get(pubkey).cloned()
    }
}
