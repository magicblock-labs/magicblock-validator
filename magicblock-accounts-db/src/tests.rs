use std::{collections::HashSet, ops::Deref, sync::Arc};

use magicblock_config::config::AccountsDbConfig;
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;
use tempfile::TempDir;

use crate::{storage::ACCOUNTS_DB_FILENAME, traits::AccountsBank, AccountsDb};

const LAMPORTS: u64 = 4425;
const SPACE: usize = 73;
const OWNER: Pubkey = Pubkey::new_from_array([23; 32]);
const ACCOUNT_DATA: &[u8] = b"hello world?";
const INIT_DATA_LEN: usize = ACCOUNT_DATA.len();
/// Default slot used for snapshots in tests.
const SNAPSHOT_SLOT: u64 = 1024;

/// Verifies basic account insertion and retrieval.
#[test]
fn test_get_account() {
    let env = TestEnv::new();
    let AccountWithPubkey { pubkey, .. } = env.create_and_insert_account();

    let acc = env.get_account(&pubkey).expect("account should exist");

    assert_eq!(acc.lamports(), LAMPORTS);
    assert_eq!(acc.owner(), &OWNER);
    assert_eq!(&acc.data()[..INIT_DATA_LEN], ACCOUNT_DATA);
    assert_eq!(acc.data().len(), SPACE);
}

/// Verifies Copy-on-Write semantics.
/// Modifying an account in memory should not affect the persistent store
/// until `upsert_account` is called.
#[test]
fn test_modify_account() {
    let env = TestEnv::new();
    let AccountWithPubkey {
        pubkey,
        account: mut uncommitted,
    } = env.create_and_insert_account();

    let new_lamports = 42;

    // Modify in memory
    assert_eq!(uncommitted.lamports(), LAMPORTS);
    uncommitted.set_lamports(new_lamports);
    assert_eq!(uncommitted.lamports(), new_lamports);

    // Verify DB is unchanged
    let committed_before = env.get_account(&pubkey).unwrap();
    assert_eq!(
        committed_before.lamports(),
        LAMPORTS,
        "database should retain old state before commit"
    );

    // Commit changes
    env.insert_account(&pubkey, &uncommitted).unwrap();

    // Verify DB is updated
    let committed_after = env.get_account(&pubkey).unwrap();
    assert_eq!(
        committed_after.lamports(),
        new_lamports,
        "database should reflect updates after commit"
    );
}

/// Verifies that accounts are correctly reallocated when their data size increases.
#[test]
fn test_account_resize() {
    let env = TestEnv::new();
    let huge_data = [42; SPACE * 4];
    let AccountWithPubkey {
        pubkey,
        account: mut uncommitted,
    } = env.create_and_insert_account();

    // Resize in memory
    uncommitted.set_data_from_slice(&huge_data);
    assert_eq!(uncommitted.data().len(), SPACE * 4);

    // Verify DB still has old size
    let committed_before = env.get_account(&pubkey).unwrap();
    assert_eq!(committed_before.data().len(), SPACE);

    // Update DB
    env.insert_account(&pubkey, &uncommitted).unwrap();

    // Verify DB has new size and data
    let committed_after = env.get_account(&pubkey).unwrap();
    assert_eq!(
        committed_after.data(),
        huge_data,
        "account data should match resized buffer"
    );
}

/// Verifies that the storage allocator reuses space (holes) created by updates.
#[test]
fn test_alloc_reuse() {
    let env = TestEnv::new();
    let AccountWithPubkey {
        pubkey: pk1,
        account: mut acc1,
    } = env.create_and_insert_account();

    // Capture the pointer address of the first allocation
    let old_ptr = env.get_account(&pk1).unwrap().data().as_ptr();

    // Resize acc1 significantly to force a move, freeing the old slot
    let huge_data = [42; SPACE * 4];
    acc1.set_data_from_slice(&huge_data);
    env.insert_account(&pk1, &acc1).unwrap();

    // Insert a new account that fits in the old slot
    let AccountWithPubkey { pubkey: pk2, .. } = env.create_and_insert_account();

    let new_ptr = env.get_account(&pk2).unwrap().data().as_ptr();

    assert_eq!(
        new_ptr, old_ptr,
        "allocator should recycle the freed slot for the new account"
    );
}

/// Verifies complex reallocation reuse logic (holes split/merge behavior).
#[test]
fn test_larger_alloc_reuse() {
    let env = TestEnv::new();

    // 1. Insert Account 1
    let mut acc1 = env.new_account_obj(SPACE);
    let huge_data_2x = vec![42; SPACE * 2];
    acc1.account.set_data_from_slice(&huge_data_2x);
    env.insert_account(&acc1.pubkey, &acc1.account).unwrap();

    // 2. Insert Account 2 (same size)
    let mut acc2 = env.new_account_obj(SPACE);
    acc2.account.set_data_from_slice(&huge_data_2x);
    env.insert_account(&acc2.pubkey, &acc2.account).unwrap();

    // 3. Insert Account 3 (4x size)
    let mut acc3 = env.new_account_obj(SPACE);
    let huge_data_4x = vec![42; SPACE * 4];
    acc3.account.set_data_from_slice(&huge_data_4x);
    env.insert_account(&acc3.pubkey, &acc3.account).unwrap();

    // Read back Account 3 to get its pointer
    let acc3_stored = env.get_account(&acc3.pubkey).unwrap();
    let acc3_ptr = acc3_stored.data().as_ptr();

    // 4. Resize Account 3 to 5x (forces move, freeing the 4x slot)
    let huge_data_5x = vec![42; SPACE * 5];
    acc3.account.set_data_from_slice(&huge_data_5x);
    env.insert_account(&acc3.pubkey, &acc3.account).unwrap();

    // 5. Insert Account 4 (3x size - fits in the 4x hole)
    let mut acc4 = env.new_account_obj(SPACE);
    let huge_data_3x = vec![42; SPACE * 3];
    acc4.account.set_data_from_slice(&huge_data_3x);
    env.insert_account(&acc4.pubkey, &acc4.account).unwrap();

    let acc4_stored = env.get_account(&acc4.pubkey).unwrap();

    assert_eq!(
        acc4_stored.data().as_ptr(),
        acc3_ptr,
        "account 4 should have reused account 3's old allocation"
    );
}

#[test]
fn test_get_program_accounts() {
    let env = TestEnv::new();
    let acc = env.create_and_insert_account();

    let accounts = env.get_program_accounts(&OWNER, |_| true);
    assert!(accounts.is_ok());

    let mut iter = accounts.unwrap();
    let (pk, data) = iter.next().unwrap();

    assert_eq!(pk, acc.pubkey);
    assert_eq!(data, acc.account);
    assert!(iter.next().is_none());
}

#[test]
fn test_get_all_accounts() {
    let env = TestEnv::new();
    let acc1 = env.create_and_insert_account();
    let acc2 = env.create_and_insert_account();
    let acc3 = env.create_and_insert_account();

    let expected_pks: HashSet<_> =
        [acc1.pubkey, acc2.pubkey, acc3.pubkey].into();

    let stored_pks: HashSet<_> = env.iter_all().map(|(pk, _)| pk).collect();

    assert_eq!(stored_pks, expected_pks);
}

#[test]
fn test_take_snapshot() {
    let env = TestEnv::new();
    let mut acc = env.create_and_insert_account();

    assert_eq!(env.slot(), 0);

    // Trigger Snapshot 1
    env.set_slot(SNAPSHOT_SLOT);
    env.take_snapshot_and_wait(SNAPSHOT_SLOT);
    assert_eq!(env.slot(), SNAPSHOT_SLOT);
    assert!(env.snapshot_exists(SNAPSHOT_SLOT));

    // Verify archive file exists (not directory)
    let archive_path = env
        .snapshot_manager
        .database_path()
        .parent()
        .unwrap()
        .join(format!("snapshot-{:0>12}.tar.gz", SNAPSHOT_SLOT));
    assert!(archive_path.exists(), "Archive file should exist");
    assert!(
        archive_path.is_file(),
        "Snapshot should be a file, not directory"
    );

    // Verify archive file exists (not directory)
    let archive_path = env
        .snapshot_manager
        .database_path()
        .parent()
        .unwrap()
        .join(format!("snapshot-{:0>12}.tar.gz", SNAPSHOT_SLOT));
    assert!(archive_path.exists(), "Archive file should exist");
    assert!(
        archive_path.is_file(),
        "Snapshot should be a file, not directory"
    );

    // Update Account
    acc.account.set_data(ACCOUNT_DATA.to_vec());
    env.insert_account(&acc.pubkey, &acc.account).unwrap();

    // Trigger Snapshot 2
    let slot2 = SNAPSHOT_SLOT * 2;
    env.set_slot(slot2);
    env.take_snapshot_and_wait(slot2);
    assert!(env.snapshot_exists(slot2));
}

/// Verifies that orphan snapshot directories are cleaned up on startup.
#[test]
fn test_orphan_directory_cleanup() {
    let (adb, temp_dir) = TestEnv::init_raw_db();

    // Create an orphan directory (simulating interrupted archiving)
    let orphan_dir = temp_dir.path().join("accountsdb/snapshot-000000000512");
    std::fs::create_dir_all(&orphan_dir).unwrap();
    std::fs::write(orphan_dir.join("test.txt"), "orphan data").unwrap();

    // Drop and reopen - orphan should be cleaned up
    drop(adb);
    let config = AccountsDbConfig::default();
    let _adb = AccountsDb::new(&config, temp_dir.path(), 0).unwrap();

    assert!(
        !orphan_dir.exists(),
        "Orphan directory should be cleaned up on startup"
    );
}

/// Verifies external snapshot fast-forward when snapshot is newer than current state.
#[test]
fn test_external_snapshot_fast_forward() {
    let env = TestEnv::new();

    // Create an account and take a local snapshot
    let acc = env.create_and_insert_account();
    env.set_slot(SNAPSHOT_SLOT);
    env.take_snapshot_and_wait(SNAPSHOT_SLOT);

    // Read the archive bytes
    let archive_path = env
        .snapshot_manager
        .database_path()
        .parent()
        .unwrap()
        .join(format!("snapshot-{:0>12}.tar.gz", SNAPSHOT_SLOT));
    let archive_bytes =
        std::fs::read(&archive_path).expect("Failed to read archive");
    let pubkey = acc.pubkey;

    // Drop current DB and create new one at slot 0
    drop(env);
    let temp_dir = tempfile::tempdir().unwrap();
    let config = AccountsDbConfig {
        reset: true,
        ..Default::default()
    };
    let mut new_db = AccountsDb::new(&config, temp_dir.path(), 0).unwrap();
    assert_eq!(new_db.slot(), 0, "New DB should start at slot 0");

    // Insert external snapshot (snapshot slot > current slot 0, should fast-forward)
    let fast_forwarded = new_db
        .insert_external_snapshot(SNAPSHOT_SLOT, &archive_bytes)
        .unwrap();
    assert!(fast_forwarded, "Should fast-forward when snapshot is newer");

    // Verify the account exists immediately after fast-forward
    let restored = new_db.get_account(&pubkey);
    assert!(
        restored.is_some(),
        "Account should exist after fast-forward"
    );
    assert_eq!(restored.unwrap().lamports(), LAMPORTS);
}

#[test]
fn test_restore_from_snapshot() {
    let mut env = TestEnv::new();
    let mut acc = env.create_and_insert_account();

    // Create Base Snapshot
    env.set_slot(SNAPSHOT_SLOT);
    env.take_snapshot_and_wait(SNAPSHOT_SLOT);

    // Make changes after snapshot
    env.set_slot(SNAPSHOT_SLOT + 3);
    let new_lamports = 999;
    acc.account.set_lamports(new_lamports);
    env.insert_account(&acc.pubkey, &acc.account).unwrap();
    env.set_slot(SNAPSHOT_SLOT + 6);

    // Verify update persisted in current state
    assert_eq!(
        env.get_account(&acc.pubkey).unwrap().lamports(),
        new_lamports
    );

    // Rollback to before the update
    env = env.restore_to_slot(SNAPSHOT_SLOT);

    let restored_acc = env.get_account(&acc.pubkey).unwrap();
    assert_eq!(
        restored_acc.lamports(),
        LAMPORTS,
        "account should be restored to state at snapshot"
    );
}

#[test]
fn test_get_all_accounts_after_rollback() {
    let mut env = TestEnv::new();
    let acc = env.create_and_insert_account();
    let mut pks = vec![acc.pubkey];
    const ITERS: u64 = 1024;

    // Create initial state
    for i in 0..=ITERS {
        let acc = env.create_and_insert_account();
        pks.push(acc.pubkey);
        env.set_slot(i);
    }

    // Take a snapshot at ITERS
    env.take_snapshot_and_wait(ITERS);

    // Add accounts after the restore point
    let mut post_snap_pks = vec![];
    for i in ITERS..ITERS + 100 {
        let acc = env.create_and_insert_account();
        env.set_slot(i + 1);
        post_snap_pks.push(acc.pubkey);
    }

    // Rollback
    env = env.restore_to_slot(ITERS);
    assert_eq!(env.slot(), ITERS);

    // Verify State
    let pubkeys: HashSet<_> = env.iter_all().map(|(pk, _)| pk).collect();

    assert_eq!(pubkeys.len(), pks.len());

    for pk in pks {
        assert!(pubkeys.contains(&pk), "Missing account {}", pk);
    }
    for pk in post_snap_pks {
        assert!(
            !pubkeys.contains(&pk),
            "Account {} should have been rolled back",
            pk
        );
    }
}

#[test]
fn test_db_size_after_rollback() {
    let mut env = TestEnv::new();
    let last_slot = 512;
    for i in 0..=last_slot {
        env.create_and_insert_account();
        env.advance_slot(i);
    }

    let pre_rollback_db_size = env.storage_size();
    let file_path = env
        .snapshot_manager
        .database_path()
        .join(ACCOUNTS_DB_FILENAME);
    let pre_rollback_file_size = file_path.metadata().unwrap().len();

    env = env.restore_to_slot(last_slot);

    assert_eq!(
        env.storage_size(),
        pre_rollback_db_size,
        "database size mismatch after rollback"
    );

    let post_rollback_file_size = file_path.metadata().unwrap().len();
    assert_eq!(
        post_rollback_file_size, pre_rollback_file_size,
        "adb file size mismatch after rollback"
    );
}

#[test]
fn test_zero_lamports_account() {
    let env = TestEnv::new();
    let mut acc = env.create_and_insert_account();

    // Explicitly set 0 lamports (simulating escrow or marker account)
    acc.account.set_lamports(0);
    env.insert_account(&acc.pubkey, &acc.account).unwrap();

    let stored = env.get_account(&acc.pubkey);
    assert!(stored.is_some(), "zero lamport account should be retained");
    assert_eq!(stored.unwrap().lamports(), 0);
}

#[test]
fn test_owner_change() {
    let env = TestEnv::new();
    let mut acc = env.create_and_insert_account();

    // Verify index before change
    assert!(matches!(
        env.account_matches_owners(&acc.pubkey, &[OWNER]),
        Some(0)
    ));

    // Change owner
    let new_owner = Pubkey::new_unique();
    acc.account.set_owner(new_owner);
    env.insert_account(&acc.pubkey, &acc.account).unwrap();

    // Verify index after change
    // Old owner should return nothing
    assert!(env.account_matches_owners(&acc.pubkey, &[OWNER]).is_none());
    assert_eq!(
        env.get_program_accounts(&OWNER, |_| true).unwrap().count(),
        0
    );

    // New owner should match
    assert!(matches!(
        env.account_matches_owners(&acc.pubkey, &[new_owner]),
        Some(0)
    ));
    assert_eq!(
        env.get_program_accounts(&new_owner, |_| true)
            .unwrap()
            .count(),
        1
    );
}

/// Verifies that we eventually hit a limit or handle capacity gracefully.
#[test]
fn test_database_full_error() {
    let env = TestEnv::new();

    // Fill DB with huge accounts
    let huge_data = vec![42; 9_000_000]; // 9MB
    let mut hit_limit = false;

    // Try to insert until failure
    for _ in 0..50 {
        let mut acc = env.new_account_obj(SPACE);
        acc.account.set_data_from_slice(&huge_data);

        if env.insert_account(&acc.pubkey, &acc.account).is_err() {
            hit_limit = true;
            break;
        }
    }

    assert!(
        hit_limit,
        "Database should eventually return error when full"
    );
}

#[test]
fn test_account_shrinking() {
    let env = TestEnv::new();
    let mut acc = env.create_and_insert_account();

    // Shrink via set_data
    acc.account.set_data(vec![]);
    env.insert_account(&acc.pubkey, &acc.account).unwrap();

    let stored = env.get_account(&acc.pubkey).unwrap();
    assert_eq!(stored.data().len(), 0);
}

#[test]
fn test_reallocation_split() {
    let env = TestEnv::new();
    const SIZE: usize = 1024;

    // Create a hole of size 2048
    let acc1 = env.create_account_with_size(SIZE * 2);
    let ptr1 = env.get_account(&acc1.pubkey).unwrap().data().as_ptr();
    env.remove_account(&acc1.pubkey); // Creates hole

    // Create 2 accounts of size 256
    let acc2 = env.create_account_with_size(SIZE / 4);
    let acc3 = env.create_account_with_size(SIZE / 4);

    let ptr2 = env.get_account(&acc2.pubkey).unwrap().data().as_ptr();
    let ptr3 = env.get_account(&acc3.pubkey).unwrap().data().as_ptr();

    // Verify they reused the space (ptr2 should be exactly at ptr1)
    assert_eq!(ptr2, ptr1, "First small account should take start of hole");
    assert!(ptr3 > ptr2, "Second small account should follow first");
}

#[test]
fn test_database_reset() {
    let (adb, temp_dir) = TestEnv::init_raw_db();
    let pubkey = Pubkey::new_unique();
    let account = AccountSharedData::new(LAMPORTS, SPACE, &OWNER);

    adb.insert_account(&pubkey, &account).unwrap();
    assert!(adb.get_account(&pubkey).is_some());

    // Explicitly drop to release locks
    drop(adb);

    // Re-open with reset=true
    let config = AccountsDbConfig {
        reset: true,
        ..Default::default()
    };

    let adb_reset = AccountsDb::new(&config, temp_dir.path(), 0).unwrap();

    assert!(adb_reset.get_account(&pubkey).is_none());
    assert_eq!(adb_reset.account_count(), 0);
}

#[test]
fn test_checksum_deterministic_across_dbs() {
    // Two independent DBs with identical accounts must produce identical checksums
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    let config = AccountsDbConfig::default();

    let db1 = AccountsDb::new(&config, dir1.path(), 0).unwrap();
    let db2 = AccountsDb::new(&config, dir2.path(), 0).unwrap();

    // Insert same accounts into both DBs
    for i in 0..50 {
        let pubkey = Pubkey::new_unique();
        let mut account = AccountSharedData::new(LAMPORTS, SPACE, &OWNER);
        account.data_as_mut_slice()[..8]
            .copy_from_slice(&(i as u64).to_le_bytes());
        db1.insert_account(&pubkey, &account).unwrap();
        db2.insert_account(&pubkey, &account).unwrap();
    }

    // Acquire write locks before computing checksums
    let lock1 = db1.write_lock();
    let lock2 = db2.write_lock();
    let _guard1 = lock1.write();
    let _guard2 = lock2.write();

    assert_eq!(
        unsafe { db1.checksum() },
        unsafe { db2.checksum() },
        "checksums must match for identical state"
    );
}

#[test]
fn test_checksum_detects_state_change() {
    let env = TestEnv::new();

    // Create initial state
    let mut accounts: Vec<_> = (0..20)
        .map(|_| {
            let acc = env.create_and_insert_account();
            (acc.pubkey, acc.account)
        })
        .collect();

    let lock = env.write_lock();
    let _guard = lock.write();
    let original_checksum = unsafe { env.checksum() };
    drop(_guard);

    // Modify a single account's data
    accounts[5].1.data_as_mut_slice()[0] ^= 0xFF;
    env.insert_account(&accounts[5].0, &accounts[5].1).unwrap();

    {
        let _guard = lock.write();
        assert_ne!(
            unsafe { env.checksum() },
            original_checksum,
            "checksum must detect single account modification"
        );
    }

    // Modify lamports on a different account
    accounts[10].1.set_lamports(1_000_000);
    env.insert_account(&accounts[10].0, &accounts[10].1)
        .unwrap();

    {
        let _guard = lock.write();
        assert_ne!(
            unsafe { env.checksum() },
            original_checksum,
            "checksum must detect lamport change"
        );
    }
}

// ==============================================================
//                      TEST UTILITIES
// ==============================================================

struct AccountWithPubkey {
    pubkey: Pubkey,
    account: AccountSharedData,
}

struct TestEnv {
    adb: Arc<AccountsDb>,
    // Kept to ensure temp dir is cleaned up on drop
    _directory: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .try_init();
        let (adb, _directory) = Self::init_raw_db();
        Self { adb, _directory }
    }

    fn init_raw_db() -> (Arc<AccountsDb>, TempDir) {
        let dir = tempfile::tempdir().expect("temp dir creation failed");
        let config = AccountsDbConfig::default();

        let adb = AccountsDb::new(&config, dir.path(), 0)
            .expect("ADB init failed")
            .into();

        (adb, dir)
    }

    fn new_account_obj(&self, size: usize) -> AccountWithPubkey {
        let pubkey = Pubkey::new_unique();
        let mut account = AccountSharedData::new(LAMPORTS, size, &OWNER);
        // Fill with some data
        if size >= INIT_DATA_LEN {
            account.data_as_mut_slice()[..INIT_DATA_LEN]
                .copy_from_slice(ACCOUNT_DATA);
        }
        AccountWithPubkey { pubkey, account }
    }

    fn create_and_insert_account(&self) -> AccountWithPubkey {
        let acc = self.new_account_obj(SPACE);
        self.adb.insert_account(&acc.pubkey, &acc.account).unwrap();
        // Re-fetch to ensure we have the stored state
        let stored = self.adb.get_account(&acc.pubkey).unwrap();
        AccountWithPubkey {
            pubkey: acc.pubkey,
            account: stored,
        }
    }

    fn create_account_with_size(&self, size: usize) -> AccountWithPubkey {
        let acc = self.new_account_obj(size);
        self.adb.insert_account(&acc.pubkey, &acc.account).unwrap();
        AccountWithPubkey {
            pubkey: acc.pubkey,
            account: acc.account,
        }
    }

    fn advance_slot(&self, target_slot: u64) {
        self.adb.set_slot(target_slot);
    }

    /// Takes a snapshot and waits for archiving to complete.
    fn take_snapshot_and_wait(&self, slot: u64) -> u64 {
        let checksum = self.adb.take_snapshot(slot);
        // Wait for background archiving to complete
        let mut retries = 0;
        while !self.adb.snapshot_exists(slot) && retries < 200 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            retries += 1;
        }
        assert!(self.adb.snapshot_exists(slot), "Snapshot should exist");
        checksum
    }

    fn restore_to_slot(mut self, slot: u64) -> Self {
        // Robustly wait for background threads (snapshots) to release the Arc.
        // Archiving can take several seconds for large databases.
        let mut retries = 0;
        let mut inner = loop {
            match Arc::try_unwrap(self.adb) {
                Ok(inner) => break inner,
                Err(adb) => {
                    if retries > 200 {
                        // Panic if still shared after ~10 seconds
                        panic!("Cannot restore: DB is shared (background snapshot thread likely still running)");
                    }
                    self.adb = adb; // Put it back to retry
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    retries += 1;
                }
            }
        };

        inner.restore_state_if_needed(slot).unwrap();
        self.adb = Arc::new(inner);
        self
    }
}

// Allow calling AccountsDb methods directly on TestEnv
impl Deref for TestEnv {
    type Target = Arc<AccountsDb>;
    fn deref(&self) -> &Self::Target {
        &self.adb
    }
}
