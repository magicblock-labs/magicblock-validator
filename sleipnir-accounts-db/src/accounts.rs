use crate::{accounts_db::AccountsDb, StorableAccounts, ZeroLamport};
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::account::ReadableAccount;
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
}
