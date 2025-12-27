use std::fs;

use magicblock_ledger::Ledger;
use solana_clock::Slot;
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::sanitized::SanitizedTransaction;
use solana_transaction_status::{
    TransactionStatusMeta, VersionedConfirmedBlock,
};
use tempfile::NamedTempFile;

pub fn setup() -> Ledger {
    let file = NamedTempFile::new().unwrap();
    let path = file.into_temp_path();
    fs::remove_file(&path).unwrap();
    Ledger::open(&path).unwrap()
}

pub fn write_dummy_transaction(
    ledger: &Ledger,
    slot: Slot,
) -> (Hash, Signature) {
    let from = Keypair::new();
    let to = Pubkey::new_unique();
    let tx =
        solana_system_transaction::transfer(&from, &to, 99, Hash::new_unique());
    let signature = Signature::new_unique();
    let transaction = SanitizedTransaction::from_transaction_for_tests(tx);
    let status = TransactionStatusMeta::default();
    let message_hash = *transaction.message_hash();
    ledger
        .write_transaction(signature, slot, &transaction, status)
        .expect("failed to write dummy transaction");

    (message_hash, signature)
}

#[allow(dead_code)]
pub fn get_block(ledger: &Ledger, slot: Slot) -> VersionedConfirmedBlock {
    ledger
        .get_block(slot)
        .expect("Failed to read ledger")
        .expect("Block not found")
}

#[allow(dead_code)]
pub fn get_block_transaction_hash(
    block: &VersionedConfirmedBlock,
    transaction_slot_index: usize,
) -> Hash {
    block
        .transactions
        .get(transaction_slot_index)
        .expect("Transaction not found in block")
        .transaction
        .message
        .hash()
}
