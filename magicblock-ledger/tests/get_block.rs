mod common;

use solana_sdk::hash::Hash;

use crate::common::{
    get_block, get_block_transaction_hash, setup, write_dummy_transaction,
};

#[test]
fn test_get_block_meta() {
    let ledger = setup();

    let slot_0_time = 5;
    let slot_1_time = slot_0_time + 1;
    let slot_2_time = slot_1_time + 1;

    let slot_0_hash = Hash::new_unique();
    let slot_1_hash = Hash::new_unique();
    let slot_2_hash = Hash::new_unique();

    assert!(ledger.write_block(0, slot_0_time, slot_0_hash).is_ok());
    assert!(ledger.write_block(1, slot_1_time, slot_1_hash).is_ok());
    assert!(ledger.write_block(2, slot_2_time, slot_2_hash).is_ok());

    let slot_0_block = get_block(&ledger, 0);
    let slot_1_block = get_block(&ledger, 1);
    let slot_2_block = get_block(&ledger, 2);

    assert_eq!(slot_0_block.block_time.unwrap(), slot_0_time);
    assert_eq!(slot_1_block.block_time.unwrap(), slot_1_time);
    assert_eq!(slot_2_block.block_time.unwrap(), slot_2_time);

    assert_eq!(slot_0_block.blockhash, slot_0_hash.to_string());
    assert_eq!(slot_1_block.blockhash, slot_1_hash.to_string());
    assert_eq!(slot_2_block.blockhash, slot_2_hash.to_string());
}

#[test]
fn test_get_block_transactions() {
    let ledger = setup();

    let (slot_41_tx1, _) = write_dummy_transaction(&ledger, 41, 0);
    let (slot_41_tx2, _) = write_dummy_transaction(&ledger, 41, 1);

    let slot_41_block_time = 410;
    let slot_41_block_hash = Hash::new_unique();
    ledger
        .write_block(41, slot_41_block_time, slot_41_block_hash)
        .unwrap();

    let (slot_42_tx1, _) = write_dummy_transaction(&ledger, 42, 0);
    let (slot_42_tx2, _) = write_dummy_transaction(&ledger, 42, 1);

    let slot_42_block_time = 420;
    let slot_42_block_hash = Hash::new_unique();
    ledger
        .write_block(42, slot_42_block_time, slot_42_block_hash)
        .unwrap();

    let block_41 = get_block(&ledger, 41);
    assert_eq!(2, block_41.transactions.len());
    assert_eq!(slot_41_tx2, get_block_transaction_hash(&block_41, 0));
    assert_eq!(slot_41_tx1, get_block_transaction_hash(&block_41, 1));

    let block_42 = get_block(&ledger, 42);
    assert_eq!(2, block_42.transactions.len());
    assert_eq!(slot_42_tx2, get_block_transaction_hash(&block_42, 0));
    assert_eq!(slot_42_tx1, get_block_transaction_hash(&block_42, 1));
}
