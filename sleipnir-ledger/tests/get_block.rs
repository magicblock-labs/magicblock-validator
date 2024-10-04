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
use solana_transaction_status::TransactionStatusMeta;
use tempfile::NamedTempFile;
use test_tools_core::init_logger;

fn setup() -> Ledger {
    let file = NamedTempFile::new().unwrap();
    let path = file.into_temp_path();
    fs::remove_file(&path).unwrap();
    Ledger::open(&path).unwrap()
}

fn write_transfer_transaction_executed(
    ledger: &Ledger,
    slot: Slot,
    from: &Keypair,
    to: &Pubkey,
    lamports: u64,
    transaction_slot_index: usize,
) -> Hash {
    let ix = system_instruction::transfer(&from.pubkey(), to, lamports);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&from.pubkey()),
        &[from],
        Hash::new_unique(),
    );
    let signature = Signature::new_unique();
    let transaction = SanitizedTransaction::from_transaction_for_tests(tx);
    let status = TransactionStatusMeta::default();
    let message_hash = transaction.message_hash().clone();
    ledger
        .write_transaction(
            signature,
            slot,
            transaction,
            status,
            transaction_slot_index,
        )
        .unwrap();
    message_hash
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

#[test]
fn test_get_block_transactions() {
    init_logger!();

    let wallet_a = Keypair::new();
    let wallet_b = Keypair::new();
    let wallet_c = Keypair::new();

    let ledger = setup();

    let slot_41_tx1 = write_transfer_transaction_executed(
        &ledger,
        41,
        &wallet_a,
        &wallet_b.pubkey(),
        241,
        1,
    );
    let slot_41_tx2 = write_transfer_transaction_executed(
        &ledger,
        41,
        &wallet_b,
        &wallet_c.pubkey(),
        141,
        2,
    );
    let slot_41_block_time = 410;
    let slot_41_block_hash = Hash::new_unique();
    ledger
        .write_block(41, slot_41_block_time, slot_41_block_hash)
        .unwrap();

    let slot_42_tx1 = write_transfer_transaction_executed(
        &ledger,
        42,
        &wallet_a,
        &wallet_b.pubkey(),
        242,
        1,
    );
    let slot_42_tx2 = write_transfer_transaction_executed(
        &ledger,
        42,
        &wallet_b,
        &wallet_c.pubkey(),
        142,
        2,
    );
    let slot_42_block_time = 420;
    let slot_42_block_hash = Hash::new_unique();
    ledger
        .write_block(42, slot_42_block_time, slot_42_block_hash)
        .unwrap();

    let block_41 = ledger.get_block(41).unwrap().unwrap();

    assert_eq!(2, block_41.transactions.len());
    assert_eq!(
        slot_41_tx2,
        block_41
            .transactions
            .get(0)
            .unwrap()
            .transaction
            .message
            .hash()
    );
    assert_eq!(
        slot_41_tx1,
        block_41
            .transactions
            .get(1)
            .unwrap()
            .transaction
            .message
            .hash()
    );

    let block_42 = ledger.get_block(42).unwrap().unwrap();

    assert_eq!(2, block_42.transactions.len());
    assert_eq!(
        slot_42_tx2,
        block_42
            .transactions
            .get(0)
            .unwrap()
            .transaction
            .message
            .hash()
    );
    assert_eq!(
        slot_42_tx1,
        block_42
            .transactions
            .get(1)
            .unwrap()
            .transaction
            .message
            .hash()
    );
}
