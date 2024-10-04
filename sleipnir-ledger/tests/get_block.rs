use std::fs;

use sleipnir_ledger::Ledger;
use solana_sdk::{
    clock::Slot,
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    system_instruction,
    transaction::{SanitizedTransaction, Transaction},
};
use solana_transaction_status::{
    TransactionStatusMeta, VersionedConfirmedBlock,
};
use tempfile::NamedTempFile;
use test_tools_core::init_logger;

fn setup() -> Ledger {
    let file = NamedTempFile::new().unwrap();
    let path = file.into_temp_path();
    fs::remove_file(&path).unwrap();
    Ledger::open(&path).unwrap()
}

fn write_dummy_transaction_executed(
    ledger: &Ledger,
    slot: Slot,
    transaction_slot_index: usize,
) -> Hash {
    let from = Keypair::new();
    let to = Pubkey::new_unique();
    let ix = system_instruction::transfer(&from.pubkey(), &to, 99);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&from.pubkey()),
        &[&from],
        Hash::new_unique(),
    );
    let signature = Signature::new_unique();
    let transaction = SanitizedTransaction::from_transaction_for_tests(tx);
    let status = TransactionStatusMeta::default();
    let message_hash = *transaction.message_hash();
    ledger
        .write_transaction(
            signature,
            slot,
            transaction,
            status,
            transaction_slot_index,
        )
        .expect("failed to write dummy transaction");
    message_hash
}

fn get_block(ledger: &Ledger, slot: Slot) -> VersionedConfirmedBlock {
    ledger
        .get_block(slot)
        .expect("Failed to read ledger")
        .expect("Block not found")
}

fn get_block_transaction_hash(
    block: &VersionedConfirmedBlock,
    transaction_index: usize,
) -> Hash {
    block
        .transactions
        .get(transaction_index)
        .expect("Transaction not found in block")
        .transaction
        .message
        .hash()
}

#[test]
fn test_get_block_meta() {
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
    init_logger!();

    let ledger = setup();

    let slot_41_tx1 = write_dummy_transaction_executed(&ledger, 41, 1);
    let slot_41_tx2 = write_dummy_transaction_executed(&ledger, 41, 2);

    let slot_41_block_time = 410;
    let slot_41_block_hash = Hash::new_unique();
    ledger
        .write_block(41, slot_41_block_time, slot_41_block_hash)
        .unwrap();

    let slot_42_tx1 = write_dummy_transaction_executed(&ledger, 42, 1);
    let slot_42_tx2 = write_dummy_transaction_executed(&ledger, 42, 2);

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
