use std::{
    collections::HashSet,
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::Arc,
};

use magicblock_config::AccountsDbConfig;
use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;

use crate::{error::AccountsDbError, storage::ADB_FILE, AccountsDb, StWLock};

const LAMPORTS: u64 = 4425;
const SPACE: usize = 73;
const OWNER: Pubkey = Pubkey::new_from_array([23; 32]);
const ACCOUNT_DATA: &[u8] = b"hello world?";
const INIT_DATA_LEN: usize = ACCOUNT_DATA.len();

const SNAPSHOT_FREQUENCY: u64 = 16;

#[test]
fn test_get_account() {
    let tenv = init_test_env();
    let AccountWithPubkey { pubkey, .. } = tenv.account();
    let acc = tenv.get_account(&pubkey);
    assert!(
        acc.is_ok(),
        "account was just inserted and should be in database"
    );
    let acc = acc.unwrap();
    assert_eq!(acc.lamports(), LAMPORTS);
    assert_eq!(acc.owner(), &OWNER);
    assert_eq!(&acc.data()[..INIT_DATA_LEN], ACCOUNT_DATA);
    assert_eq!(acc.data().len(), SPACE);
}

#[test]
fn test_modify_account() {
    let tenv = init_test_env();
    let AccountWithPubkey {
        pubkey,
        account: mut uncommitted,
    } = tenv.account();

    let new_lamports = 42;

    assert_eq!(uncommitted.lamports(), LAMPORTS);
    uncommitted.set_lamports(new_lamports);
    assert_eq!(uncommitted.lamports(), new_lamports);

    let mut committed = tenv
        .get_account(&pubkey)
        .expect("account should be in database");

    assert_eq!(
        committed.lamports(),
        LAMPORTS,
        "account from the main buffer should not be affected"
    );
    tenv.insert_account(&pubkey, &uncommitted);

    committed = tenv
        .get_account(&pubkey)
        .expect("account should be in database");

    assert_eq!(
        committed.lamports(),
        new_lamports,
        "account's main buffer should have been switched after commit"
    );
}

#[test]
fn test_account_resize() {
    let tenv = init_test_env();
    let huge_data = [42; SPACE * 4];
    let AccountWithPubkey {
        pubkey,
        account: mut uncommitted,
    } = tenv.account();

    uncommitted.set_data_from_slice(&huge_data);
    assert!(
        matches!(uncommitted, AccountSharedData::Owned(_),),
        "account should have been promoted to Owned after resize"
    );
    assert_eq!(
        uncommitted.data().len(),
        SPACE * 4,
        "account should have been resized to double of SPACE"
    );

    let mut committed = tenv
        .get_account(&pubkey)
        .expect("account should be in database");

    assert_eq!(
        committed.data().len(),
        SPACE,
        "uncommitted account data len should not have changed"
    );

    tenv.insert_account(&pubkey, &uncommitted);

    committed = tenv
        .get_account(&pubkey)
        .expect("account should be in database");

    assert_eq!(
        committed.data(),
        huge_data,
        "account should have been resized after insertion"
    );
}

#[test]
fn test_alloc_reuse() {
    let tenv = init_test_env();
    let AccountWithPubkey {
        pubkey,
        account: mut acc1,
    } = tenv.account();
    let huge_data = [42; SPACE * 4];

    let old_addr = acc1.data().as_ptr();

    acc1.set_data_from_slice(&huge_data);
    tenv.insert_account(&pubkey, &acc1);

    let AccountWithPubkey { account: acc2, .. } = tenv.account();

    assert_eq!(
        acc2.data().as_ptr(),
        old_addr,
        "new account insertion should have reused the allocation"
    );

    let AccountWithPubkey { account: acc3, .. } = tenv.account();

    assert!(
        acc3.data().as_ptr() > acc2.data().as_ptr(),
        "last account insertion should have been freshly allocated"
    );
}

#[test]
fn test_larger_alloc_reuse() {
    let tenv = init_test_env();
    let mut acc = tenv.account();

    let mut huge_data = vec![42; SPACE * 2];
    acc.account.set_data_from_slice(&huge_data);
    tenv.insert_account(&acc.pubkey, &acc.account);

    let mut acc2 = tenv.account();
    acc2.account.set_data_from_slice(&huge_data);
    tenv.insert_account(&acc2.pubkey, &acc2.account);

    let mut acc3 = tenv.account();
    huge_data = vec![42; SPACE * 4];
    acc3.account.set_data_from_slice(&huge_data);
    tenv.insert_account(&acc3.pubkey, &acc3.account);
    acc3.account = tenv
        .get_account(&acc3.pubkey)
        .expect("third account should be in database");

    let alloc_addr = acc3.account.data().as_ptr();
    huge_data = vec![42; SPACE * 5];
    acc3.account.set_data_from_slice(&huge_data);
    tenv.insert_account(&acc3.pubkey, &acc3.account);

    let mut acc4 = tenv.account();
    huge_data = vec![42; SPACE * 3];
    acc4.account.set_data_from_slice(&huge_data);
    tenv.insert_account(&acc4.pubkey, &acc4.account);
    acc4.account = tenv
        .get_account(&acc4.pubkey)
        .expect("fourth account should be in database");

    assert_eq!(
        acc4.account.data().as_ptr(),
        alloc_addr,
        "fourth account should have reused the allocation from third one"
    );
}

#[test]
fn test_get_program_accounts() {
    let tenv = init_test_env();
    let acc = tenv.account();
    let accounts = tenv.get_program_accounts(&OWNER, |_| true);
    assert!(accounts.is_ok(), "program account should be in database");
    let mut accounts = accounts.unwrap();
    assert_eq!(accounts.len(), 1, "one program account has been inserted");
    assert_eq!(
        accounts.pop().unwrap().1,
        acc.account,
        "returned program account should match inserted one"
    );
}

#[test]
fn test_get_all_accounts() {
    let tenv = init_test_env();
    let acc = tenv.account();
    let mut pubkeys = HashSet::new();
    pubkeys.insert(acc.pubkey);
    let acc2 = tenv.account();
    tenv.insert_account(&acc2.pubkey, &acc2.account);
    pubkeys.insert(acc2.pubkey);
    let acc3 = tenv.account();
    tenv.insert_account(&acc3.pubkey, &acc3.account);
    pubkeys.insert(acc3.pubkey);

    let mut pks = tenv.iter_all();
    assert!(pks
        .next()
        .map(|(pk, _)| pubkeys.contains(&pk))
        .unwrap_or_default());
    assert!(pks
        .next()
        .map(|(pk, _)| pubkeys.contains(&pk))
        .unwrap_or_default());
    assert!(pks
        .next()
        .map(|(pk, _)| pubkeys.contains(&pk))
        .unwrap_or_default());
    assert!(pks.next().is_none());
}

#[test]
fn test_take_snapshot() {
    let tenv = init_test_env();
    let mut acc = tenv.account();

    assert_eq!(tenv.slot(), 0, "fresh accountsdb should have 0 slot");
    tenv.set_slot(SNAPSHOT_FREQUENCY);
    assert_eq!(
        tenv.slot(),
        SNAPSHOT_FREQUENCY,
        "adb slot must have been updated"
    );
    assert!(
        tenv.snapshot_exists(SNAPSHOT_FREQUENCY),
        "first snapshot should have been created"
    );
    acc.account.set_data(ACCOUNT_DATA.to_vec());

    tenv.insert_account(&acc.pubkey, &acc.account);

    tenv.set_slot(2 * SNAPSHOT_FREQUENCY);
    assert!(
        tenv.snapshot_exists(2 * SNAPSHOT_FREQUENCY),
        "second snapshot should have been created"
    );
}

#[test]
fn test_restore_from_snapshot() {
    let mut tenv = init_test_env();
    let mut acc = tenv.account();
    let new_lamports = 42;

    tenv.set_slot(SNAPSHOT_FREQUENCY); // trigger snapshot
    tenv.set_slot(SNAPSHOT_FREQUENCY + 1);
    acc.account.set_lamports(new_lamports);
    tenv.insert_account(&acc.pubkey, &acc.account);

    let acc_committed = tenv
        .get_account(&acc.pubkey)
        .expect("account should be in database");
    assert_eq!(
        acc_committed.lamports(),
        new_lamports,
        "account's lamports should have been updated after commit"
    );
    tenv.set_slot(SNAPSHOT_FREQUENCY * 3);

    assert!(
        matches!(
            tenv.ensure_at_most(SNAPSHOT_FREQUENCY * 2),
            Ok(SNAPSHOT_FREQUENCY)
        ),
        "failed to rollback to snapshot"
    );

    let acc_rolledback = tenv
        .get_account(&acc.pubkey)
        .expect("account should be in database");
    assert_eq!(
        acc_rolledback.lamports(),
        LAMPORTS,
        "account's lamports should have been rolled back"
    );
    assert_eq!(tenv.slot(), SNAPSHOT_FREQUENCY);
}

#[test]
fn test_get_all_accounts_after_rollback() {
    let mut tenv = init_test_env();
    let acc = tenv.account();
    let mut pks = vec![acc.pubkey];
    const ITERS: u64 = 1024;
    for i in 0..=ITERS {
        let acc = tenv.account();
        tenv.insert_account(&acc.pubkey, &acc.account);
        pks.push(acc.pubkey);
        tenv.set_slot(i);
    }

    let mut post_snap_pks = vec![];
    for i in ITERS..ITERS + SNAPSHOT_FREQUENCY {
        let acc = tenv.account();
        tenv.insert_account(&acc.pubkey, &acc.account);
        tenv.set_slot(i + 1);
        post_snap_pks.push(acc.pubkey);
    }

    assert!(
        matches!(tenv.ensure_at_most(ITERS), Ok(ITERS)),
        "failed to rollback to snapshot"
    );

    let asserter = |(pk, acc): (_, AccountSharedData)| {
        assert_eq!(
            acc.data().len(),
            SPACE,
            "account was incorrectly deserialized"
        );
        assert_eq!(
            &acc.data()[..INIT_DATA_LEN],
            ACCOUNT_DATA,
            "account data contains garbage"
        );
        pk
    };
    let pubkeys = tenv.iter_all().map(asserter).collect::<HashSet<_>>();

    assert_eq!(pubkeys.len(), pks.len());

    for pk in pks {
        assert!(pubkeys.contains(&pk));
    }
    for pk in post_snap_pks {
        assert!(!pubkeys.contains(&pk));
    }
}

#[test]
fn test_db_size_after_rollback() {
    let mut tenv = init_test_env();
    let last_slot = 512;
    for i in 0..=last_slot {
        let acc = tenv.account();
        tenv.insert_account(&acc.pubkey, &acc.account);
        tenv.set_slot(i);
    }
    let pre_rollback_db_size = tenv.storage_size();
    let path = tenv.snapshot_engine.database_path();
    let adb_file = path.join(ADB_FILE);
    let pre_rollback_file_size = adb_file
        .metadata()
        .expect("failed to get metadata for adb file")
        .len();

    tenv.ensure_at_most(last_slot)
        .expect("failed to rollback accounts database");

    assert_eq!(
        tenv.storage_size(),
        pre_rollback_db_size,
        "database size mismatch after rollback"
    );
    let path = tenv.snapshot_engine.database_path();
    let adb_file = path.join(ADB_FILE);
    let post_rollback_len = adb_file
        .metadata()
        .expect("failed to get metadata for adb file")
        .len();
    assert_eq!(
        post_rollback_len, pre_rollback_file_size,
        "adb file size mismatch after rollback"
    );
}

#[test]
fn test_account_removal() {
    let tenv = init_test_env();
    let mut acc = tenv.account();
    let pk = acc.pubkey;
    assert!(
        tenv.get_account(&pk).is_ok(),
        "account should exists after init"
    );

    acc.account.set_lamports(0);

    tenv.insert_account(&pk, &acc.account);

    assert!(
        matches!(tenv.get_account(&pk), Err(AccountsDbError::NotFound)),
        "account should have been deleted after lamports have been zeroed out"
    );
}

#[test]
fn test_owner_change() {
    let tenv = init_test_env();
    let mut acc = tenv.account();
    let result = tenv.account_matches_owners(&acc.pubkey, &[OWNER]);
    assert!(matches!(result, Ok(0)));
    let mut accounts = tenv
        .get_program_accounts(&OWNER, |_| true)
        .expect("failed to get program accounts");
    let expected = (acc.pubkey, acc.account.clone());
    assert_eq!(accounts.pop(), Some(expected));

    let new_owner = Pubkey::new_unique();
    acc.account.set_owner(new_owner);
    tenv.insert_account(&acc.pubkey, &acc.account);
    let result = tenv.account_matches_owners(&acc.pubkey, &[OWNER]);
    assert!(matches!(result, Err(AccountsDbError::NotFound)));
    let result = tenv.get_program_accounts(&OWNER, |_| true);
    assert!(result.map(|pks| pks.is_empty()).unwrap_or_default());

    let result = tenv.account_matches_owners(&acc.pubkey, &[OWNER, new_owner]);
    assert!(matches!(result, Ok(1)));
    accounts = tenv
        .get_program_accounts(&new_owner, |_| true)
        .expect("failed to get program accounts");
    assert_eq!(accounts.pop().map(|(k, _)| k), Some(acc.pubkey));
}

#[test]
#[should_panic]
fn test_account_too_many_accounts() {
    let tenv = init_test_env();
    for _ in 0..20 {
        let acc = tenv.account();
        let mut oversized_account = acc.account;
        oversized_account.extend_from_slice(&[42; 9_000_000]);
        tenv.insert_account(&acc.pubkey, &oversized_account);
    }
}

#[test]
fn test_account_shrinking() {
    let tenv = init_test_env();
    let mut acc1 = tenv.account();

    // ==============================================
    // test set_data
    acc1.account.set_data(b"".to_vec());
    tenv.insert_account(&acc1.pubkey, &acc1.account);
    acc1.account = tenv
        .get_account(&acc1.pubkey)
        .expect("account should be inserted");
    assert_eq!(
        acc1.account.data().len(),
        0,
        "account data should have been truncated"
    );

    // ==============================================
    // test set_data_from_slice
    let mut acc2 = tenv.account();
    tenv.insert_account(&acc2.pubkey, &acc2.account);

    acc2.account = tenv
        .get_account(&acc2.pubkey)
        .expect("account 2 should be inserted");

    acc2.account.set_data_from_slice(b"");

    tenv.insert_account(&acc2.pubkey, &acc2.account);
    acc2.account = tenv
        .get_account(&acc2.pubkey)
        .expect("account should be inserted");
    assert_eq!(
        acc2.account.data().len(),
        0,
        "account data should have been truncated"
    );

    // ==============================================
    // test set_data_from_slice
    let mut acc3 = tenv.account();
    tenv.insert_account(&acc3.pubkey, &acc3.account);

    acc3.account = tenv
        .get_account(&acc3.pubkey)
        .expect("account 2 should be inserted");

    acc3.account.resize(0, 0);

    tenv.insert_account(&acc3.pubkey, &acc3.account);
    acc3.account = tenv
        .get_account(&acc3.pubkey)
        .expect("account should be inserted");
    assert_eq!(
        acc3.account.data().len(),
        0,
        "account data should have been truncated"
    );
}

#[test]
fn test_many_insertions_to_accountsdb() {
    const ACCOUNTNUM: usize = 16384;
    const ITERS: usize = 2 << 16;
    const THREADNUM: usize = 4;

    let tenv = init_test_env();

    let mut pubkeys = Vec::with_capacity(ACCOUNTNUM);
    for _ in 0..ACCOUNTNUM {
        let acc = tenv.account();
        pubkeys.push(acc.pubkey);
        tenv.insert_account(&acc.pubkey, &acc.account);
    }
    // test whether frequent account reallocations effectively reuse free
    // space in database without overflowing the database boundaries (100MB for test)
    let tenv_arc = Arc::new(tenv);
    let chunksize = ACCOUNTNUM / THREADNUM;
    std::thread::scope(|s| {
        for pks in pubkeys.chunks(chunksize) {
            let tenv_arc = tenv_arc.clone();
            s.spawn(move || {
                for i in 0..ITERS {
                    let pk = &pks[i % chunksize];
                    let mut account = tenv_arc
                        .get_account(pk)
                        .expect("account should be in database");
                    account
                        .set_data_from_slice(&vec![43; i % (SPACE * 20) + 13]);
                    tenv_arc.insert_account(pk, &account);
                }
            });
        }
    });
}

// ==============================================================
// ==============================================================
//                      UTILITY CODE BELOW
// ==============================================================
// ==============================================================

struct AccountWithPubkey {
    pubkey: Pubkey,
    account: AccountSharedData,
}

struct AdbTestEnv {
    adb: AccountsDb,
    directory: PathBuf,
}

pub fn init_db() -> (AccountsDb, PathBuf) {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .is_test(true)
        .try_init();
    let directory = tempfile::tempdir()
        .expect("failed to create temporary directory")
        .keep();
    let config = AccountsDbConfig::temp_for_tests(SNAPSHOT_FREQUENCY);
    let lock = StWLock::default();

    let adb = AccountsDb::new(&config, &directory, lock)
        .expect("expected to initialize ADB");
    (adb, directory)
}

fn init_test_env() -> AdbTestEnv {
    let (adb, directory) = init_db();
    AdbTestEnv { adb, directory }
}

impl AdbTestEnv {
    fn account(&self) -> AccountWithPubkey {
        let pubkey = Pubkey::new_unique();
        let mut account = AccountSharedData::new(LAMPORTS, SPACE, &OWNER);
        account.data_as_mut_slice()[..INIT_DATA_LEN]
            .copy_from_slice(ACCOUNT_DATA);
        self.adb.insert_account(&pubkey, &account);
        let account = self
            .get_account(&pubkey)
            .expect("failed to refetch newly inserted account");
        AccountWithPubkey { pubkey, account }
    }
}

impl Deref for AdbTestEnv {
    type Target = AccountsDb;
    fn deref(&self) -> &Self::Target {
        &self.adb
    }
}

impl DerefMut for AdbTestEnv {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.adb
    }
}

impl Drop for AdbTestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
