use std::fs;

use sleipnir_ledger::Ledger;
use solana_sdk::hash::Hash;
use tempfile::NamedTempFile;
use test_tools_core::init_logger;

pub fn setup() -> Ledger {
    let file = NamedTempFile::new().unwrap();
    let path = file.into_temp_path();
    fs::remove_file(&path).unwrap();
    Ledger::open(&path).unwrap()
}

#[test]
fn test_persist_block_meta() {
    init_logger!();

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

    assert_eq!(
        ledger.get_block(0).unwrap().unwrap().block_time.unwrap(),
        slot_0_time
    );
    assert_eq!(
        ledger.get_block(1).unwrap().unwrap().block_time.unwrap(),
        slot_1_time
    );
    assert_eq!(
        ledger.get_block(2).unwrap().unwrap().block_time.unwrap(),
        slot_2_time
    );

    assert_eq!(
        ledger.get_block(0).unwrap().unwrap().blockhash,
        slot_0_hash.to_string()
    );
    assert_eq!(
        ledger.get_block(1).unwrap().unwrap().blockhash,
        slot_1_hash.to_string()
    );
    assert_eq!(
        ledger.get_block(2).unwrap().unwrap().blockhash,
        slot_2_hash.to_string()
    );
}
