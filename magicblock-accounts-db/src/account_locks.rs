use std::collections::{hash_map, HashMap, HashSet};

use solana_frozen_abi_macro::AbiExample;
use solana_sdk::pubkey::Pubkey;

#[derive(Debug, Default, AbiExample)]
pub struct AccountLocks {
    pub(crate) write_locks: HashSet<Pubkey>,
    pub(crate) readonly_locks: HashMap<Pubkey, u64>,
}

impl AccountLocks {
    pub(crate) fn is_locked_readonly(&self, key: &Pubkey) -> bool {
        self.readonly_locks
            .get(key)
            .map_or(false, |count| *count > 0)
    }

    pub(crate) fn is_locked_write(&self, key: &Pubkey) -> bool {
        self.write_locks.contains(key)
    }

    pub(crate) fn insert_new_readonly(&mut self, key: &Pubkey) {
        assert!(self.readonly_locks.insert(*key, 1).is_none());
    }

    pub(crate) fn lock_readonly(&mut self, key: &Pubkey) -> bool {
        self.readonly_locks.get_mut(key).map_or(false, |count| {
            *count += 1;
            true
        })
    }

    pub(crate) fn unlock_readonly(&mut self, key: &Pubkey) {
        if let hash_map::Entry::Occupied(mut occupied_entry) =
            self.readonly_locks.entry(*key)
        {
            let count = occupied_entry.get_mut();
            *count -= 1;
            if *count == 0 {
                occupied_entry.remove_entry();
            }
        }
    }

    pub(crate) fn unlock_write(&mut self, key: &Pubkey) {
        self.write_locks.remove(key);
    }
}
