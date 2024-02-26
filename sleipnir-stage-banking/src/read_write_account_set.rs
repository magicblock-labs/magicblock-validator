// NOTE: from core/src/banking_stage/read_write_account_set.rs
use {
    solana_sdk::{message::SanitizedMessage, pubkey::Pubkey},
    std::collections::HashSet,
};

/// Wrapper struct to accumulate locks for a batch of transactions.
#[derive(Debug, Default)]
pub struct ReadWriteAccountSet {
    /// Set of accounts that are locked for read
    read_set: HashSet<Pubkey>,
    /// Set of accounts that are locked for write
    write_set: HashSet<Pubkey>,
}

impl ReadWriteAccountSet {
    /// Returns true if all account locks were available and false otherwise.
    #[allow(dead_code)]
    pub fn check_locks(&self, message: &SanitizedMessage) -> bool {
        message
            .account_keys()
            .iter()
            .enumerate()
            .all(|(index, pubkey)| {
                if message.is_writable(index) {
                    self.can_write(pubkey)
                } else {
                    self.can_read(pubkey)
                }
            })
    }

    /// Add all account locks.
    /// Returns true if all account locks were available and false otherwise.
    pub fn take_locks(&mut self, message: &SanitizedMessage) -> bool {
        message
            .account_keys()
            .iter()
            .enumerate()
            .fold(true, |all_available, (index, pubkey)| {
                if message.is_writable(index) {
                    all_available & self.add_write(pubkey)
                } else {
                    all_available & self.add_read(pubkey)
                }
            })
    }

    /// Clears the read and write sets
    pub fn clear(&mut self) {
        self.read_set.clear();
        self.write_set.clear();
    }

    /// Check if an account can be read-locked
    fn can_read(&self, pubkey: &Pubkey) -> bool {
        !self.write_set.contains(pubkey)
    }

    /// Check if an account can be write-locked
    fn can_write(&self, pubkey: &Pubkey) -> bool {
        !self.write_set.contains(pubkey) && !self.read_set.contains(pubkey)
    }

    /// Add an account to the read-set.
    /// Returns true if the lock was available.
    fn add_read(&mut self, pubkey: &Pubkey) -> bool {
        let can_read = self.can_read(pubkey);
        self.read_set.insert(*pubkey);

        can_read
    }

    /// Add an account to the write-set.
    /// Returns true if the lock was available.
    fn add_write(&mut self, pubkey: &Pubkey) -> bool {
        let can_write = self.can_write(pubkey);
        self.write_set.insert(*pubkey);

        can_write
    }
}

#[cfg(test)]
mod tests {
    // NOTE: removed some tests that had dependency on ledger
    use {super::ReadWriteAccountSet, solana_sdk::pubkey::Pubkey};

    #[test]
    pub fn test_write_write_conflict() {
        let mut account_locks = ReadWriteAccountSet::default();
        let account = Pubkey::new_unique();
        assert!(account_locks.can_write(&account));
        account_locks.add_write(&account);
        assert!(!account_locks.can_write(&account));
    }

    #[test]
    pub fn test_read_write_conflict() {
        let mut account_locks = ReadWriteAccountSet::default();
        let account = Pubkey::new_unique();
        assert!(account_locks.can_read(&account));
        account_locks.add_read(&account);
        assert!(!account_locks.can_write(&account));
        assert!(account_locks.can_read(&account));
    }

    #[test]
    pub fn test_write_read_conflict() {
        let mut account_locks = ReadWriteAccountSet::default();
        let account = Pubkey::new_unique();
        assert!(account_locks.can_write(&account));
        account_locks.add_write(&account);
        assert!(!account_locks.can_write(&account));
        assert!(!account_locks.can_read(&account));
    }

    #[test]
    pub fn test_read_read_non_conflict() {
        let mut account_locks = ReadWriteAccountSet::default();
        let account = Pubkey::new_unique();
        assert!(account_locks.can_read(&account));
        account_locks.add_read(&account);
        assert!(account_locks.can_read(&account));
    }

    #[test]
    pub fn test_write_write_different_keys() {
        let mut account_locks = ReadWriteAccountSet::default();
        let account1 = Pubkey::new_unique();
        let account2 = Pubkey::new_unique();
        assert!(account_locks.can_write(&account1));
        account_locks.add_write(&account1);
        assert!(account_locks.can_write(&account2));
        assert!(account_locks.can_read(&account2));
    }
}
