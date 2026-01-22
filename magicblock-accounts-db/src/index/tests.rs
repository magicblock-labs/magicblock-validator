use std::{
    ops::Deref,
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use lmdb::Transaction;
use magicblock_config::config::AccountsDbConfig;
use solana_pubkey::Pubkey;
use tempfile::TempDir;

use super::{AccountsDbIndex, Allocation};
use crate::error::AccountsDbError;

/// Verifies that `upsert_account` correctly handles both new insertions
/// and updates to existing accounts, returning the old allocation when applicable.
#[test]
fn test_upsert_account() {
    let env = IndexTestEnv::new();
    let account = env.new_account();

    // 1. First Insertion (New Account)
    let mut txn = env.rw_txn();
    let result = env.upsert_account(&account, &mut txn);
    txn.commit().expect("commit failed");

    assert!(result.is_ok(), "failed to insert account");
    assert!(
        result.unwrap().is_none(),
        "new account should not have a previous allocation"
    );

    // 2. Re-insertion (Update Account)
    let mut txn = env.rw_txn();
    let new_allocation = env.new_allocation();

    // We manually call the internal method to inject a specific new allocation
    let result = env.index.upsert_account(
        &account.pubkey,
        &account.owner,
        new_allocation,
        &mut txn,
    );
    txn.commit().expect("commit failed");

    assert!(result.is_ok(), "failed to update account");
    let previous = result.unwrap();
    assert_eq!(
        previous,
        Some(account.allocation.into()),
        "update should return the old allocation for recycling"
    );
}

/// Verifies we can retrieve the correct storage offset for an account
/// using both the convenience method and raw transaction lookup.
#[test]
fn test_get_offset() {
    let env = IndexTestEnv::new();
    let account = env.new_account();

    env.persist_account(&account);

    // Test high-level convenience method
    let offset = env.get_offset(&account.pubkey);
    assert_eq!(offset.unwrap(), account.allocation.offset);

    // Test low-level internal retrieval
    let txn = env.rw_txn();
    let allocation = env.get_allocation(&txn, &account.pubkey);
    assert_eq!(allocation.unwrap(), account.allocation.into());
}

/// Ensures that `reallocate_account` moves the account record, updates the index,
/// and marks the old space as deallocated.
#[test]
fn test_reallocate_account() {
    let env = IndexTestEnv::new();
    let account = env.new_account();
    env.persist_account(&account);

    let mut txn = env.rw_txn();
    let new_allocation = env.new_allocation();

    let new_index_value = bytes!(
        #pack,
        new_allocation.offset, u32,
        new_allocation.blocks, u32
    );

    let result =
        env.reallocate_account(&account.pubkey, &mut txn, &new_index_value);
    txn.commit().expect("commit failed");

    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        account.allocation.into(),
        "should return old allocation"
    );

    // Verify persistence of new offset
    let stored_offset = env.get_offset(&account.pubkey).unwrap();
    assert_eq!(stored_offset, new_allocation.offset);
}

#[test]
fn test_remove_account() {
    let env = IndexTestEnv::new();
    let account = env.new_account();
    env.persist_account(&account);

    // Perform removal
    let result = env.remove(&account.pubkey);
    assert!(result.is_ok());

    // Verify Index Entry is gone
    let offset = env.get_offset(&account.pubkey);
    assert!(matches!(offset, Err(AccountsDbError::NotFound)));

    // Verify Space was Deallocated
    assert_eq!(env.count_deallocations(), 1, "deallocations count mismatch");
}

/// Tests the owner consistency check.
/// If an account's owner changes, the `programs` index must be updated
/// to reflect the move from the old owner to the new owner.
#[test]
fn test_ensure_correct_owner() {
    let env = IndexTestEnv::new();
    let account = env.new_account();
    env.persist_account(&account);

    // Verify initial state
    let programs: Vec<_> = env
        .get_program_accounts_iter(&account.owner)
        .unwrap()
        .collect();
    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0], (account.allocation.offset, account.pubkey));

    // Change Owner
    let new_owner = Pubkey::new_unique();
    let mut txn = env.rw_txn();
    let result =
        env.ensure_correct_owner(&account.pubkey, &new_owner, &mut txn);
    txn.commit().expect("commit failed");
    assert!(result.is_ok());

    // Verify old owner is empty
    let old_programs_count = env
        .get_program_accounts_iter(&account.owner)
        .unwrap()
        .count();
    assert_eq!(old_programs_count, 0);

    // Verify new owner has account
    let new_programs: Vec<_> =
        env.get_program_accounts_iter(&new_owner).unwrap().collect();
    assert_eq!(new_programs.len(), 1);
    assert_eq!(new_programs[0], (account.allocation.offset, account.pubkey));
}

#[test]
fn test_program_index_cleanup() {
    let env = IndexTestEnv::new();
    let account = env.new_account();
    env.persist_account(&account);

    let mut txn = env.rw_txn();
    let result = env.remove_program_index_entry(
        &account.pubkey,
        None,
        &mut txn,
        account.allocation.offset,
    );
    txn.commit().expect("commit failed");
    assert!(result.is_ok());

    // Verify cleanup
    let count = env
        .get_program_accounts_iter(&account.owner)
        .unwrap()
        .count();
    assert_eq!(count, 0);
}

/// Verifies the full cycle of:
/// Insert -> Reallocate (creates hole) -> Insert New (fills hole).
#[test]
fn test_recycle_allocation_flow() {
    let env = IndexTestEnv::new();
    let account = env.new_account();

    // 1. Insert
    env.persist_account(&account);

    // 2. Reallocate (moves account, creating a hole at `account.allocation`)
    let mut txn = env.rw_txn();
    let new_allocation = env.new_allocation();
    let index_val =
        bytes!(#pack, new_allocation.offset, u32, new_allocation.blocks, u32);
    env.reallocate_account(&account.pubkey, &mut txn, &index_val)
        .unwrap();
    txn.commit().expect("commit failed");

    assert_eq!(env.count_deallocations(), 1, "hole created");

    // 3. Recycle exact fit
    // We request a block size exactly matching the hole we just made.
    let mut txn = env.rw_txn();
    let recycled =
        env.try_recycle_allocation(account.allocation.blocks, &mut txn);
    txn.commit().expect("commit failed");

    assert!(recycled.is_ok());
    assert_eq!(
        recycled.unwrap(),
        account.allocation.into(),
        "should reuse the exact hole"
    );
    assert_eq!(env.count_deallocations(), 0, "hole consumed");

    // 4. Recycle missing (should fail)
    let mut txn = env.rw_txn();
    let missing = env.try_recycle_allocation(100, &mut txn);
    assert!(matches!(missing, Err(AccountsDbError::NotFound)));
}

/// Verifies that requesting a smaller size than an available hole
/// correctly splits the hole and returns the remainder to the free list.
#[test]
fn test_recycle_allocation_split() {
    let env = IndexTestEnv::new();
    let account = env.new_account();
    env.persist_account(&account);
    env.remove(&account.pubkey).unwrap();

    assert_eq!(env.count_deallocations(), 1);

    let half_size = account.allocation.blocks / 2;

    // 1. Recycle first half
    let mut txn = env.rw_txn();
    let part1 = env.try_recycle_allocation(half_size, &mut txn).unwrap();
    txn.commit().expect("commit failed");

    assert_eq!(part1.blocks, half_size);
    assert_eq!(part1.offset, account.allocation.offset);

    // Should still have one hole (the remainder)
    assert_eq!(env.count_deallocations(), 1);

    // 2. Recycle second half (remainder)
    let mut txn = env.rw_txn();
    let part2 = env.try_recycle_allocation(half_size, &mut txn).unwrap();
    txn.commit().expect("commit failed");

    assert_eq!(part2.blocks, half_size);
    // The new offset should be shifted by the size of the first part
    assert!(part2.offset > account.allocation.offset);

    // Empty now
    assert_eq!(env.count_deallocations(), 0);
}

#[test]
fn test_byte_pack_unpack_macro() {
    macro_rules! check_pack {
        ($v1: expr, $t1: ty, $v2: expr, $t2: ty) => {{
            let packed = bytes!(#pack, $v1, $t1, $v2, $t2);
            let (u1, u2) = bytes!(#unpack, packed, $t1, $t2);
            assert_eq!($v1, u1);
            assert_eq!($v2, u2);
        }};
    }

    check_pack!(13u8, u8, 42u64, u64);
    check_pack!(12345u32, u32, 67890u32, u32);

    let pubkey = Pubkey::new_unique();
    check_pack!(100u32, u32, pubkey, Pubkey);
    check_pack!(pubkey, Pubkey, 255u8, u8);
}

// ==============================================================
//                      TEST UTILITIES
// ==============================================================

struct IndexTestEnv {
    index: AccountsDbIndex,
    // Kept to ensure directory deletion on drop
    _temp_dir: TempDir,
}

struct IndexAccount {
    pubkey: Pubkey,
    owner: Pubkey,
    allocation: Allocation,
}

impl IndexTestEnv {
    fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .try_init();
        let _temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let index = AccountsDbIndex::new(
            AccountsDbConfig::default().index_size,
            _temp_dir.path(),
        )
        .expect("failed to create index");

        Self { index, _temp_dir }
    }

    /// Opens a read-write transaction. Panics on failure.
    fn rw_txn(&self) -> lmdb::RwTransaction<'_> {
        self.index
            .env
            .begin_rw_txn()
            .expect("failed to begin rw txn")
    }

    /// Generates a new dummy allocation with a unique offset.
    fn new_allocation(&self) -> Allocation {
        static OFFSET_COUNTER: AtomicU32 = AtomicU32::new(0);
        let blocks = 4;
        // Mocking an offset allocator
        let offset = OFFSET_COUNTER.fetch_add(blocks, Ordering::Relaxed);

        Allocation {
            ptr: NonNull::dangling(),
            offset,
            blocks,
        }
    }

    /// Generates a random account with a dummy allocation.
    fn new_account(&self) -> IndexAccount {
        IndexAccount {
            pubkey: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            allocation: self.new_allocation(),
        }
    }

    /// Helper to insert an account into the index immediately.
    /// Useful for setting up test state.
    fn persist_account(&self, acc: &IndexAccount) {
        let mut txn = self.rw_txn();
        self.index
            .upsert_account(&acc.pubkey, &acc.owner, acc.allocation, &mut txn)
            .expect("persist failed");
        txn.commit().expect("persist commit failed");
    }

    /// Helper to count entries in the private deallocations table.
    fn count_deallocations(&self) -> usize {
        let txn = self.index.env.begin_ro_txn().unwrap();
        self.index.deallocations.entries(&txn)
    }

    /// Helper wrapper to call upsert on the internal account struct.
    fn upsert_account(
        &self,
        acc: &IndexAccount,
        txn: &mut lmdb::RwTransaction,
    ) -> crate::AccountsDbResult<Option<crate::storage::ExistingAllocation>>
    {
        self.index
            .upsert_account(&acc.pubkey, &acc.owner, acc.allocation, txn)
    }
}

// Enable calling Index methods directly on Env
impl Deref for IndexTestEnv {
    type Target = AccountsDbIndex;
    fn deref(&self) -> &Self::Target {
        &self.index
    }
}
