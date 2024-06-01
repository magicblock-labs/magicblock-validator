use crate::accounts_db::AccountsDb;
use crate::accounts_index::ZeroLamport;
use crate::storable_accounts::StorableAccounts;
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    pubkey::Pubkey,
};
use std::sync::Arc;

#[derive(Debug, AbiExample)]
pub struct Accounts {
    /// Single global AccountsDb
    pub accounts_db: Arc<AccountsDb>,
    // NOTE: we may need the account locks here
}

impl Accounts {
    pub fn new(accounts_db: Arc<AccountsDb>) -> Self {
        Self { accounts_db }
    }

    pub fn store_accounts_cached<
        'a,
        T: ReadableAccount + Sync + ZeroLamport + 'a,
    >(
        &self,
        accounts: impl StorableAccounts<'a, T>,
    ) {
        self.accounts_db.store_cached(accounts, None)
    }

    pub fn load_with_slot(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, Slot)> {
        self.accounts_db.load_with_slot(pubkey)
    }
}
