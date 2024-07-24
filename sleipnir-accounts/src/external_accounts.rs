use std::{
    collections::HashMap,
    ops::Deref,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::Duration,
};

use conjunto_transwise::CommitFrequency;
use solana_sdk::{clock::Slot, pubkey::Pubkey};

use crate::utils::get_epoch;

// -----------------
// ExternalAccounts
// -----------------
pub trait ExternalAccount {
    fn cloned_from_slot(&self) -> Slot;
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
    pub fn has(&self, pubkey: &Pubkey) -> bool {
        self.read_accounts().contains_key(pubkey)
    }

    pub fn is_empty(&self) -> bool {
        self.read_accounts().is_empty()
    }

    pub fn len(&self) -> usize {
        self.read_accounts().len()
    }

    pub fn get_cloned_at(&self, pubkey: &Pubkey) -> Option<Duration> {
        self.read_accounts()
            .get(pubkey)
            .map(|account| account.cloned_at())
    }

    pub fn get_cloned_from_slot(&self, pubkey: &Pubkey) -> Option<Slot> {
        self.read_accounts()
            .get(pubkey)
            .map(|account| account.cloned_from_slot())
    }

    pub fn read_accounts(&self) -> RwLockReadGuard<HashMap<Pubkey, T>> {
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
    /// The pubkey of the account.
    pub pubkey: Pubkey,
    /// The main-chain slot at which the account was cloned from
    pub cloned_from_slot: Slot,
    /// The timestamp at which the account was cloned into the validator.
    pub cloned_at: Duration,
    pub updated_at: Duration,
}

impl ExternalAccount for ExternalReadonlyAccount {
    fn cloned_from_slot(&self) -> Slot {
        self.cloned_from_slot
    }
    fn cloned_at(&self) -> Duration {
        self.cloned_at
    }
}

impl ExternalReadonlyAccount {
    fn new(pubkey: Pubkey, from_slot: Slot, now: Duration) -> Self {
        Self {
            pubkey,
            cloned_from_slot: from_slot,
            cloned_at: now,
            updated_at: now,
        }
    }
}

impl ExternalReadonlyAccounts {
    pub fn insert(&self, pubkey: Pubkey, from_slot: Slot) {
        let now = get_epoch();
        self.write_accounts().insert(
            pubkey,
            ExternalReadonlyAccount::new(pubkey, from_slot, now),
        );
    }

    pub fn remove(&self, pubkey: &Pubkey) {
        self.write_accounts().remove(pubkey);
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

impl ExternalWritableAccounts {
    pub fn insert(
        &self,
        pubkey: Pubkey,
        from_slot: Slot,
        commit_frequency: Option<CommitFrequency>,
    ) {
        let now = get_epoch();
        self.write_accounts().insert(
            pubkey,
            ExternalWritableAccount::new(
                pubkey,
                from_slot,
                now,
                commit_frequency,
            ),
        );
    }
}

#[derive(Debug)]
pub struct ExternalWritableAccount {
    /// The pubkey of the account.
    pub pubkey: Pubkey,
    /// The main-chain slot at which the account was cloned from
    cloned_from_slot: Slot,
    /// The timestamp at which the account was cloned into the validator.
    cloned_at: Duration,
    /// The frequency at which to commit the state to the commit buffer of
    /// the locked account.
    /// This is `None` for accounts that are not locked, i.e. for payers.
    /// If it is `None` we don't need to commit the account ever.
    commit_frequency: Option<Duration>,
    /// The timestamp at which the account was last committed.
    last_committed_at: RwLock<Duration>,
}

impl ExternalAccount for ExternalWritableAccount {
    fn cloned_from_slot(&self) -> u64 {
        self.cloned_from_slot
    }
    fn cloned_at(&self) -> Duration {
        self.cloned_at
    }
}

impl ExternalWritableAccount {
    fn new(
        pubkey: Pubkey,
        from_slot: Slot,
        now: Duration,
        commit_frequency: Option<CommitFrequency>,
    ) -> Self {
        let commit_frequency = commit_frequency.map(Duration::from);
        Self {
            pubkey,
            commit_frequency,
            cloned_from_slot: from_slot,
            cloned_at: now,
            // We don't want to commit immediately after cloning, thus we consider
            // the account as committed at clone time until it is updated after
            // a commit
            last_committed_at: RwLock::new(now),
        }
    }

    pub fn needs_commit(&self, now: Duration) -> bool {
        let commit_frequency = if let Some(freq) = self.commit_frequency {
            freq
        } else {
            // accounts like payers without commit frequency are never committed
            return false;
        };
        let last_committed_at = *self
            .last_committed_at
            .read()
            .expect("RwLock of last_committed_at is poisoned");

        now - last_committed_at >= commit_frequency
    }

    pub fn mark_as_committed(&self, now: Duration) {
        *self
            .last_committed_at
            .write()
            .expect("RwLock of last_committed_at is poisoned") = now
    }

    pub fn last_committed_at(&self) -> Duration {
        *self
            .last_committed_at
            .read()
            .expect("RwLock of last_committed_at is poisoned")
    }
}
