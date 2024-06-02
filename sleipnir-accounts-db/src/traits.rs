use solana_sdk::{account::ReadableAccount, clock::Slot, pubkey::Pubkey};

// -----------------
// ZeroLamport
// -----------------
pub trait ZeroLamport {
    fn is_zero_lamport(&self) -> bool;
}

// -----------------
// IsCached
// -----------------
pub trait IsCached {
    fn is_cached(&self) -> bool;
}

// -----------------
// StorableAccounts
// -----------------

/// abstract access to pubkey, account, slot, target_slot of either:
/// a. (slot, &[&Pubkey, &ReadableAccount])
/// b. (slot, &[&Pubkey, &ReadableAccount, Slot]) (we will use this later)
/// This trait avoids having to allocate redundant data when there is a duplicated slot parameter.
/// All legacy callers do not have a unique slot per account to store.
pub trait StorableAccounts<'a, T: ReadableAccount + Sync>: Sync {
    /// pubkey at 'index'
    fn pubkey(&self, index: usize) -> &Pubkey;
    /// account at 'index'
    fn account(&self, index: usize) -> &T;
    /// None if account is zero lamports
    fn account_default_if_zero_lamport(&self, index: usize) -> Option<&T> {
        let account = self.account(index);
        (account.lamports() != 0).then_some(account)
    }
    /// current slot for account at 'index'
    fn slot(&self, index: usize) -> Slot;
    /// slot that all accounts are to be written to
    fn target_slot(&self) -> Slot;
    /// true if no accounts to write
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// # accounts to write
    fn len(&self) -> usize;
    /// are there accounts from multiple slots
    /// only used for an assert
    fn contains_multiple_slots(&self) -> bool {
        false
    }

    // NOTE: left out hash/write_version related methods
}
