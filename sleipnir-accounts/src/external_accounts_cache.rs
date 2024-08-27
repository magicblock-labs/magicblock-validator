use std::{
    collections::HashMap,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use conjunto_transwise::AccountChainSnapshotShared;
use solana_sdk::{clock::Slot, pubkey::Pubkey};

#[derive(Debug)]
enum ExternalAccountCacheMeta {
    Delegated {},
}

#[derive(Debug)]
struct ExternalAccountCacheItem {
    snapshot: AccountChainSnapshotShared,
    meta: ExternalAccountCacheMeta,
}

#[derive(Debug, Default)]
pub struct ExternalAccountsCache {
    accounts: RwLock<HashMap<Pubkey, ExternalAccountCacheItem>>,
}

impl ExternalAccountsCache {
    fn accounts_read_lock(
        &self,
    ) -> RwLockReadGuard<HashMap<Pubkey, ExternalAccountCacheItem>> {
        self.accounts
            .read()
            .expect("RwLock of ExternalAccountsCache.accounts is poisoned")
    }
    fn accounts_write_lock(
        &self,
    ) -> RwLockWriteGuard<HashMap<Pubkey, ExternalAccountCacheItem>> {
        self.accounts
            .write()
            .expect("RwLock of ExternalAccountsCache.accounts is poisoned")
    }
}
