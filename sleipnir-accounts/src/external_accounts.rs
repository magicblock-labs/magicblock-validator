use std::{
    collections::HashMap,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::Duration,
};

use conjunto_transwise::AccountChainSnapshot;
use solana_sdk::{clock::Slot, pubkey::Pubkey};

pub struct ExternalAccountFetched {
    /// The pubkey of the account.
    pub pubkey: Pubkey,
    /// The main-chain slot at which the account was cloned
    pub fetched_at_slot: Slot,
    /// The timestamp at which the account was cloned into the validator.
    pub fetched_at: Duration,
    // The main-chain data fetched
    pub chain_snapshot: AccountChainSnapshot,
    /// The timestamp at which the account was last committed.
    pub committed_at: RwLock<Duration>,
}

#[derive(Debug)]
pub struct ExternalAccountsStore {
    accounts: RwLock<HashMap<Pubkey, u64>>,
}

impl ExternalAccountsStore {
    fn read_accounts(&self) -> RwLockReadGuard<HashMap<Pubkey, u64>> {
        self.accounts
            .read()
            .expect("RwLock of ExternalAccountsStore.accounts is poisoned")
    }
    fn write_accounts(&self) -> RwLockWriteGuard<HashMap<Pubkey, u64>> {
        self.accounts
            .write()
            .expect("RwLock of ExternalAccountsStore.accounts is poisoned")
    }

    /*
    pub fn insert(&self, pubkey: Pubkey, at_slot: Slot) {
        let now = get_epoch();
        self.write_accounts()
            .insert(pubkey, ExternalReadonlyAccount::new(pubkey, at_slot, now));
    }

    pub fn remove(&self, pubkey: &Pubkey) {
        self.write_accounts().remove(pubkey);
    } */
}

/*
impl ExternalWritableAccount {
    fn new(
        pubkey: Pubkey,
        at_slot: Slot,
        now: Duration,
        commit_frequency: Option<CommitFrequency>,
    ) -> Self {
        let commit_frequency = commit_frequency.map(Duration::from);
        Self {
            pubkey,
            commit_frequency,
            cloned_at_slot: at_slot,
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
*/
