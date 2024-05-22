use std::{
    collections::HashMap,
    ops::Deref,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::Duration,
};

use solana_sdk::pubkey::Pubkey;

use crate::utils::get_epoch;

// -----------------
// ExternalAccounts
// -----------------
pub trait ExternalAccount {
    fn new(pubkey: Pubkey, now: Duration) -> Self;
    fn cloned_at(&self) -> Duration;
}

#[derive(Debug)]
pub struct ExternalAccounts<T: ExternalAccount> {
    accounts: RwLock<HashMap<Pubkey, T>>,
}

impl<T: ExternalAccount> Default for ExternalAccounts<T> {
    fn default() -> Self {
        Self {
            accounts: RwLock::new(HashMap::new()),
        }
    }
}

impl<T: ExternalAccount> ExternalAccounts<T> {
    pub fn insert(&self, pubkey: Pubkey) {
        let now = get_epoch();
        self.write_accounts().insert(pubkey, T::new(pubkey, now));
    }

    pub fn has(&self, pubkey: &Pubkey) -> bool {
        self.read_accounts().contains_key(pubkey)
    }

    pub fn is_empty(&self) -> bool {
        self.read_accounts().is_empty()
    }

    pub fn len(&self) -> usize {
        self.read_accounts().len()
    }

    pub fn cloned_at(&self, pubkey: &Pubkey) -> Option<Duration> {
        self.read_accounts()
            .get(pubkey)
            .map(|account| account.cloned_at())
    }

    fn read_accounts(&self) -> RwLockReadGuard<HashMap<Pubkey, T>> {
        self.accounts
            .read()
            .expect("RwLock of external accounts is poisoned")
    }

    fn write_accounts(&self) -> RwLockWriteGuard<HashMap<Pubkey, T>> {
        self.accounts
            .write()
            .expect("RwLock of external accounts is poisoned")
    }
}

// -----------------
// ExternalReadonlyAccounts
// -----------------
#[derive(Default, Debug)]
pub struct ExternalReadonlyAccounts(ExternalAccounts<ExternalReadonlyAccount>);

impl Deref for ExternalReadonlyAccounts {
    type Target = ExternalAccounts<ExternalReadonlyAccount>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug)]
pub struct ExternalReadonlyAccount {
    pub pubkey: Pubkey,
    pub cloned_at: Duration,
    pub updated_at: Duration,
}

impl ExternalAccount for ExternalReadonlyAccount {
    fn new(pubkey: Pubkey, now: Duration) -> Self {
        Self {
            pubkey,
            cloned_at: now,
            updated_at: now,
        }
    }

    fn cloned_at(&self) -> Duration {
        self.cloned_at
    }
}

impl ExternalReadonlyAccounts {
    pub fn get_updated_at(&self, pubkey: &Pubkey) -> Option<Duration> {
        self.read_accounts()
            .get(pubkey)
            .map(|account| account.updated_at)
    }
}

// -----------------
// ExternalWritableAccounts
// -----------------
#[derive(Default, Debug)]
pub struct ExternalWritableAccounts(ExternalAccounts<ExternalWritableAccount>);

impl Deref for ExternalWritableAccounts {
    type Target = ExternalAccounts<ExternalWritableAccount>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug)]
pub struct ExternalWritableAccount {
    pub pubkey: Pubkey,
    pub cloned_at: Duration,
    pub updated_at: Duration,
    pub last_committed_at: Option<Duration>,
}

impl ExternalAccount for ExternalWritableAccount {
    fn new(pubkey: Pubkey, now: Duration) -> Self {
        Self {
            pubkey,
            cloned_at: now,
            updated_at: now,
            last_committed_at: None,
        }
    }

    fn cloned_at(&self) -> Duration {
        self.cloned_at
    }
}
