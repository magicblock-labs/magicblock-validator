use std::{
    fs, ops::Deref, path::PathBuf, ptr::NonNull, sync::atomic::AtomicU32,
};

use lmdb::Transaction;
use magicblock_config::{AccountsDbConfig, BlockSize, TEST_SNAPSHOT_FREQUENCY};
use solana_pubkey::Pubkey;

use super::{AccountsDbIndex, Allocation};
use crate::error::AccountsDbError;

#[test]
fn test_insert_account() {
    let tenv = setup();
    let IndexAccount {
        pubkey,
        owner,
        allocation,
    } = tenv.account();

    let result = tenv.insert_account(&pubkey, &owner, allocation);
    assert!(result.is_ok(), "failed to insert account into index");
    assert!(
        result.unwrap().is_none(),
        "new account should not be reallocated"
    );
    let reallocation = tenv.allocation();
    let result = tenv.insert_account(&pubkey, &owner, reallocation);
    assert!(result.is_ok(), "failed to RE-insert account into index");
    let previous_allocation = allocation.into();
    assert_eq!(
        result.unwrap(),
        Some(previous_allocation),
        "account RE-insertion should return previous allocation"
    );
}

#[test]
fn test_get_account_offset() {
    let tenv = setup();
    let IndexAccount {
        pubkey,
        owner,
        allocation,
    } = tenv.account();

    tenv.insert_account(&pubkey, &owner, allocation)
        .expect("failed to insert account");
    let result = tenv.get_account_offset(&pubkey);
    assert!(result.is_ok(), "failed to read offset for inserted account");
    assert_eq!(
        result.unwrap(),
        allocation.offset,
        "offset of read account doesn't match that of written one"
    );

    let txn = tenv
        .env
        .begin_rw_txn()
        .expect("failed to start new RW transaction");

    let result = tenv.get_allocation(&txn, &pubkey);

    assert!(
        result.is_ok(),
        "failed to read an allocation for inserted account"
    );
    assert_eq!(
        result.unwrap(),
        allocation.into(),
        "allocation of account doesn't match one which was defined during insertion"
    );
}

#[test]
fn test_reallocate_account() {
    let tenv = setup();
    let IndexAccount {
        pubkey,
        owner,
        allocation,
    } = tenv.account();

    tenv.insert_account(&pubkey, &owner, allocation)
        .expect("failed to insert account");

    let mut txn = tenv
        .env
        .begin_rw_txn()
        .expect("failed to start new RW transaction");

    let new_allocation = tenv.allocation();
    let index_value =
        bytes!(#pack, new_allocation.offset, u32, new_allocation.blocks, u32);
    let result = tenv.reallocate_account(&pubkey, &mut txn, &index_value);

    txn.commit().expect("failed to commit transaction");

    assert!(result.is_ok(), "failed to reallocate account");
    assert_eq!(
        result.unwrap(),
        allocation.into(),
        "allocation of account doesn't match one, which was defined during insertion"
    );
    let result = tenv
        .get_account_offset(&pubkey)
        .expect("failed to read reallocated account");
    assert_eq!(
        result, new_allocation.offset,
        "reallocated account's offset doesn't match new allocation"
    );
}

#[test]
fn test_remove_account() {
    let tenv = setup();
    let IndexAccount {
        pubkey,
        owner,
        allocation,
    } = tenv.account();

    tenv.insert_account(&pubkey, &owner, allocation)
        .expect("failed to insert account");

    let result = tenv.remove_account(&pubkey);

    assert!(result.is_ok(), "failed to remove account");
    let offset = tenv.get_account_offset(&pubkey);
    assert!(
        matches!(offset, Err(AccountsDbError::NotFound)),
        "removed account offset is still present in index"
    );
    assert_eq!(
        tenv.get_delloactions_count(),
        1,
        "the number of deallocations should have increased after account removal"
    );
}

#[test]
fn test_ensure_correct_owner() {
    let tenv = setup();
    let IndexAccount {
        pubkey,
        owner,
        allocation,
    } = tenv.account();

    tenv.insert_account(&pubkey, &owner, allocation)
        .expect("failed to insert account");
    let iter = tenv.get_program_accounts_iter(&owner);
    assert!(
        iter.is_ok(),
        "failed to get iterator for newly inserted program account"
    );
    let mut iter = iter.unwrap();
    assert_eq!(
        iter.next(),
        Some((allocation.offset, pubkey)),
        "account returned by program iterator is invalid one"
    );
    assert_eq!(
        iter.next(),
        None,
        "program iterator returned more than the number of inserted accounts"
    );
    drop(iter);

    let new_owner = Pubkey::new_unique();
    assert!(
        tenv.ensure_correct_owner(&pubkey, &new_owner).is_ok(),
        "failed to ensure correct account owner"
    );
    let result = tenv.get_program_accounts_iter(&owner);
    assert!(
        matches!(result.map(|i| i.count()), Ok(0)),
        "programs index still has record of account after owner change"
    );

    let mut iter = tenv
        .get_program_accounts_iter(&new_owner)
        .expect("failed to get iterator for newly inserted program account");
    assert_eq!(
        iter.next(),
        Some((allocation.offset, pubkey)),
        "account returned by program iterator is invalid one"
    );
}

#[test]
fn test_program_index_cleanup() {
    let tenv = setup();
    let IndexAccount {
        pubkey,
        owner,
        allocation,
    } = tenv.account();

    tenv.insert_account(&pubkey, &owner, allocation)
        .expect("failed to insert account");

    let mut txn = tenv
        .env
        .begin_rw_txn()
        .expect("failed to start new RW transaction");
    let result = tenv.remove_programs_index_entry(
        &pubkey,
        None,
        &mut txn,
        allocation.offset,
    );
    assert!(result.is_ok(), "failed to remove entry from programs index");
    txn.commit().expect("failed to commit transaction");

    let result = tenv.get_program_accounts_iter(&owner);
    assert!(
        matches!(result.map(|i| i.count()), Ok(0)),
        "programs index still has record of account after cleanup"
    );
}

#[test]
fn test_recycle_allocation_after_realloc() {
    let tenv = setup();
    let IndexAccount {
        pubkey,
        owner,
        allocation,
    } = tenv.account();

    tenv.insert_account(&pubkey, &owner, allocation)
        .expect("failed to insert account");

    let mut txn = tenv
        .env
        .begin_rw_txn()
        .expect("failed to start new RW transaction");
    let new_allocation = tenv.allocation();
    let index_value =
        bytes!(#pack, new_allocation.offset, u32, new_allocation.blocks, u32);
    tenv.reallocate_account(&pubkey, &mut txn, &index_value)
        .expect("failed to reallocate account");
    txn.commit().expect("failed to commit transaction");
    assert_eq!(
        tenv.get_delloactions_count(),
        1,
        "the number of deallocations should have increased after account realloc"
    );

    let result = tenv.try_recycle_allocation(new_allocation.blocks);
    assert_eq!(
        result.expect("failed to recycle allocation"),
        allocation.into()
    );
    assert_eq!(
        tenv.get_delloactions_count(),
        0,
        "the number of deallocations should have decresed after recycling"
    );
    let result = tenv.try_recycle_allocation(new_allocation.blocks);
    assert!(
        matches!(result, Err(AccountsDbError::NotFound)),
        "deallocations index should have run out of existing allocations"
    );
    tenv.remove_account(&pubkey)
        .expect("failed to remove account");
    assert_eq!(
        tenv.get_delloactions_count(),
        1,
        "the number of deallocations should have increased after account removal"
    );
    let result = tenv.try_recycle_allocation(new_allocation.blocks);
    assert_eq!(
        result.expect("failed to recycle allocation after account removal"),
        new_allocation.into()
    );
    assert_eq!(
        tenv.get_delloactions_count(),
        0,
        "the number of deallocations should have decresed after recycling"
    );
}

#[test]
fn test_byte_pack_unpack_macro() {
    macro_rules! check {
        ($v1: expr, $t1: ty, $v2: expr, $t2: ty, $tranformer: ident) => {
            check!($v1, $t1, $v2, $t2, $tranformer, $tranformer);
        };
        ($v1: expr, $t1: ty, $v2: expr, $t2: ty, $tranformer1: ident, $tranformer2: ident) => {{
            // get the cummulative size of value 1 and value 2, as they are laid out in memory
            const S1: usize = size_of::<$t1>();
            const S2: usize = size_of::<$t2>();
            // create a buffer array to hold both values in concatenated form
            let mut expected = [0_u8; S1 + S2];

            // put the first value to S1 bytes of buffer, by using type to bytes transformer
            expected[..S1].copy_from_slice(<$t1>::$tranformer1($v1).as_slice());
            // put the second value to S2 bytes of buffer following S1 bytes, by using type to bytes transformer
            expected[S1..].copy_from_slice(<$t2>::$tranformer2($v2).as_slice());

            // pack/concatenate the values together
            let result = bytes!(#pack, $v1, $t1, $v2, $t2);

            // manually serialized buffer array should match the array produced by bytes! macro
            assert_eq!(
                result,
                expected,
                "invalid byte packing of {} ({}) and {} ({})",
                $v1, stringify!($t1), $v2, stringify!($t2)
            );
            // now, undo the whole thing by unpacking the array back to constituent types
            let (v1, v2) = bytes!(#unpack, result, $t1, $t2);

            // we should get exactly the same values and types for the first value
            assert_eq!(
                $v1, v1, "unpacked value 1 doesn't match with initial {} <> {v1} ({})",
                $v1, stringify!($t1)
            );
            // same goes for the second value
            assert!(
                $v2.eq(&v2), "unpacked value 2 doesn't match with initial {} <> {v2} ({})",
                $v2, stringify!($t2)
            );
        }};
    }

    check!(13, u8, 42, i64, to_le_bytes);
    check!(13, i8, 42, u8, to_le_bytes);
    check!(13, u16, 42, i8, to_le_bytes);
    check!(13, i16, 42, u16, to_le_bytes);
    check!(13, u32, 42, i16, to_le_bytes);
    check!(13, i32, 42, u32, to_le_bytes);
    check!(13, u64, 42, i32, to_le_bytes);
    check!(13, i64, 42, u64, to_le_bytes);

    let pubkey = Pubkey::new_unique();

    check!(13, u8, pubkey, Pubkey, to_le_bytes, to_bytes);
    check!(13, i8, pubkey, Pubkey, to_le_bytes, to_bytes);
    check!(13, u16, pubkey, Pubkey, to_le_bytes, to_bytes);
    check!(13, i16, pubkey, Pubkey, to_le_bytes, to_bytes);
    check!(13, u32, pubkey, Pubkey, to_le_bytes, to_bytes);
    check!(13, i32, pubkey, Pubkey, to_le_bytes, to_bytes);
    check!(13, u64, pubkey, Pubkey, to_le_bytes, to_bytes);
    check!(13, i64, pubkey, Pubkey, to_le_bytes, to_bytes);

    check!(pubkey, Pubkey, 13, u8, to_bytes, to_le_bytes);
    check!(pubkey, Pubkey, 13, i8, to_bytes, to_le_bytes);
    check!(pubkey, Pubkey, 13, u16, to_bytes, to_le_bytes);
    check!(pubkey, Pubkey, 13, i16, to_bytes, to_le_bytes);
    check!(pubkey, Pubkey, 13, u32, to_bytes, to_le_bytes);
    check!(pubkey, Pubkey, 13, i32, to_bytes, to_le_bytes);
    check!(pubkey, Pubkey, 13, u64, to_bytes, to_le_bytes);
    check!(pubkey, Pubkey, 13, i64, to_bytes, to_le_bytes);
}

// ==============================================================
// ==============================================================
//                      UTILITY CODE BELOW
// ==============================================================
// ==============================================================

fn setup() -> IndexTestEnv {
    let config = AccountsDbConfig::temp_for_tests(TEST_SNAPSHOT_FREQUENCY);
    let directory = tempfile::tempdir()
        .expect("failed to create temp directory for index tests")
        .keep();
    let index = AccountsDbIndex::new(config.index_map_size, &directory)
        .expect("failed to create accountsdb index in temp dir");
    IndexTestEnv { index, directory }
}

struct IndexTestEnv {
    index: AccountsDbIndex,
    directory: PathBuf,
}

struct IndexAccount {
    pubkey: Pubkey,
    owner: Pubkey,
    allocation: Allocation,
}

impl IndexTestEnv {
    fn allocation(&self) -> Allocation {
        static ALLOCATION: AtomicU32 =
            AtomicU32::new(BlockSize::Block256 as u32);
        let blocks = 4;
        let offset = ALLOCATION.fetch_add(
            (BlockSize::Block256 as u32) * blocks,
            std::sync::atomic::Ordering::Relaxed,
        );

        Allocation {
            storage: NonNull::dangling(),
            offset,
            blocks,
        }
    }
    fn account(&self) -> IndexAccount {
        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let allocation = self.allocation();
        IndexAccount {
            pubkey,
            owner,
            allocation,
        }
    }
}

impl Deref for IndexTestEnv {
    type Target = AccountsDbIndex;
    fn deref(&self) -> &Self::Target {
        &self.index
    }
}

impl Drop for IndexTestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
