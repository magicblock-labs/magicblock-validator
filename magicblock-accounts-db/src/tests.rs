use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
use solana_pubkey::Pubkey;

use crate::{config::AdbConfig, AccountsDb, StWLock};

const LAMPORTS: u64 = 4425;
const SPACE: usize = 73;
const OWNER: Pubkey = Pubkey::new_from_array([23; 32]);
const ACCOUNT_DATA: &[u8] = b"hello world?";
const INIT_DATA_LEN: usize = ACCOUNT_DATA.len();

const SNAPSHOT_FREQUENCY: u64 = 16;

#[test]
fn test_get_account() {
    let adb = init_db();
    let (pubkey, account) = account();
    adb.insert_account(&pubkey, &account);
    let acc = adb.get_account(&pubkey);
    assert!(
        acc.is_ok(),
        "account was just inserted and should be in database"
    );
    let acc = AccountSharedData::Borrowed(acc.unwrap());
    assert_eq!(acc.lamports(), LAMPORTS);
    assert_eq!(acc.owner(), &OWNER);
    assert_eq!(&acc.data()[..INIT_DATA_LEN], ACCOUNT_DATA);
    assert_eq!(acc.data().len(), SPACE);
}

#[test]
fn test_modify_account() {
    let DbWithAcc { adb, acc } = init_db_with_acc();
    let mut acc_uncommitted = acc.account;
    let new_lamports = 42;

    assert_eq!(acc_uncommitted.lamports(), LAMPORTS);
    acc_uncommitted.set_lamports(new_lamports);
    assert_eq!(acc_uncommitted.lamports(), new_lamports);

    let acc_committed = AccountSharedData::Borrowed(
        adb.get_account(&acc.pubkey)
            .expect("account should be in database"),
    );
    assert_eq!(
        acc_committed.lamports(),
        LAMPORTS,
        "account from the main buffer should not be affected"
    );
    adb.insert_account(&acc.pubkey, &acc_uncommitted);

    let acc_committed = AccountSharedData::Borrowed(
        adb.get_account(&acc.pubkey)
            .expect("account should be in database"),
    );
    assert_eq!(
        acc_committed.lamports(),
        new_lamports,
        "account's main buffer should have been switched after commit"
    );
}

#[test]
fn test_account_resize() {
    let DbWithAcc { adb, mut acc } = init_db_with_acc();
    let huge_date = [42; SPACE * 2];

    acc.account.set_data_from_slice(&huge_date);
    assert!(
        matches!(acc.account, AccountSharedData::Owned(_),),
        "account should have been promoted to Owned after resize"
    );

    let acc_committed = AccountSharedData::Borrowed(
        adb.get_account(&acc.pubkey)
            .expect("account should be in database"),
    );

    assert_eq!(
        acc_committed.data().len(),
        SPACE,
        "unccomitted account data len should not have changed"
    );

    adb.insert_account(&acc.pubkey, &acc.account);

    let acc_committed = AccountSharedData::Borrowed(
        adb.get_account(&acc.pubkey)
            .expect("account should be in database"),
    );

    assert_eq!(
        acc_committed.data(),
        huge_date,
        "account should have been resized after insertion"
    );
}

#[test]
fn test_alloc_reuse() {
    let DbWithAcc { adb, mut acc } = init_db_with_acc();
    let huge_date = [42; SPACE * 2];

    acc.account.set_data_from_slice(&huge_date);

    let acc_committed = AccountSharedData::Borrowed(
        adb.get_account(&acc.pubkey)
            .expect("account should be in database"),
    );
    let old_addr = acc_committed.data().as_ptr();

    adb.insert_account(&acc.pubkey, &acc.account);
    let (pk2, acc2) = account();
    adb.insert_account(&pk2, &acc2);

    let acc_alloc_reused = AccountSharedData::Borrowed(
        adb.get_account(&pk2)
            .expect("second account should be in database"),
    );

    assert_eq!(
        acc_alloc_reused.data().as_ptr(),
        old_addr,
        "new account insertion should have reused the allocation"
    );
}

#[test]
fn test_get_program_accounts() {
    let DbWithAcc { adb, acc } = init_db_with_acc();
    let accounts = adb.get_program_accounts(&OWNER, |_| true);
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
fn test_take_snapshot() {
    let DbWithAcc { adb, mut acc } = init_db_with_acc();

    assert_eq!(adb.slot(), 0, "fresh accountsdb should have 0 slot");
    adb.set_slot(SNAPSHOT_FREQUENCY);
    assert_eq!(
        adb.slot(),
        SNAPSHOT_FREQUENCY,
        "adb slot must have been updated"
    );
    assert!(
        adb.snapshot_exists(SNAPSHOT_FREQUENCY),
        "first snapshot should have been created"
    );
    acc.account.set_data(ACCOUNT_DATA.to_vec());

    adb.insert_account(&acc.pubkey, &acc.account);

    adb.set_slot(2 * SNAPSHOT_FREQUENCY);
    assert!(
        adb.snapshot_exists(2 * SNAPSHOT_FREQUENCY),
        "second snapshot should have been created"
    );
}

#[test]
fn test_restore_from_snapshot() {
    let DbWithAcc { adb, mut acc } = init_db_with_acc();
    let new_lamports = 42;

    adb.set_slot(SNAPSHOT_FREQUENCY); // trigger snapshot
    adb.set_slot(SNAPSHOT_FREQUENCY + 1);
    acc.account.set_lamports(new_lamports);
    adb.insert_account(&acc.pubkey, &acc.account);

    let acc_committed = AccountSharedData::Borrowed(
        adb.get_account(&acc.pubkey)
            .expect("account should be in database"),
    );
    assert_eq!(
        acc_committed.lamports(),
        new_lamports,
        "account's lamports should have been updated after commit"
    );

    assert!(
        matches!(
            unsafe { adb.ensure_at_most(SNAPSHOT_FREQUENCY) }
                .inspect(|d| println!("S: {d}"))
                .inspect_err(|e| println!("E: {e}")),
            Ok(SNAPSHOT_FREQUENCY)
        ),
        "failed to rollback to snapshot"
    );

    let acc_rolledback = AccountSharedData::Borrowed(
        adb.get_account(&acc.pubkey)
            .expect("account should be in database"),
    );
    assert_eq!(
        acc_rolledback.lamports(),
        LAMPORTS,
        "account's lamports should have been rolled back"
    );
}

// ==============================================================
// ==============================================================

struct AccountWithPubkey {
    pubkey: Pubkey,
    account: AccountSharedData,
}

struct DbWithAcc {
    adb: AccountsDb,
    acc: AccountWithPubkey,
}

pub fn init_db() -> AccountsDb {
    let _ = env_logger::builder().is_test(true).try_init();
    let config = AdbConfig::temp_for_tests(SNAPSHOT_FREQUENCY);
    let lock = StWLock::default();

    AccountsDb::new(&config, lock).expect("expected to initialize ADB")
}

fn init_db_with_acc() -> DbWithAcc {
    let adb = init_db();
    let (pubkey, account) = account();
    adb.insert_account(&pubkey, &account);
    let acc = adb
        .get_account(&pubkey)
        .expect("account retrieval should be successful");
    let acc = AccountWithPubkey {
        account: AccountSharedData::Borrowed(acc),
        pubkey,
    };

    DbWithAcc { adb, acc }
}

fn account() -> (Pubkey, AccountSharedData) {
    let pubkey = Pubkey::new_unique();
    let mut account = AccountSharedData::new(LAMPORTS, SPACE, &OWNER);
    account.data_as_mut_slice()[..INIT_DATA_LEN].copy_from_slice(ACCOUNT_DATA);
    (pubkey, account)
}
