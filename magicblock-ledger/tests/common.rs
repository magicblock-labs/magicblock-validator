use std::fs;

use magicblock_ledger::Ledger;
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

pub fn setup() -> Ledger {
    let file = NamedTempFile::new().unwrap();
    let path = file.into_temp_path();
    fs::remove_file(&path).unwrap();
    Ledger::open(&path).unwrap()
}

pub fn write_dummy_transaction(
    ledger: &Ledger,
    slot: Slot,
    transaction_slot_index: usize,
) -> (Hash, Signature) {
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

    (message_hash, signature)
}

pub fn get_block(ledger: &Ledger, slot: Slot) -> VersionedConfirmedBlock {
    ledger
        .get_block(slot)
        .expect("Failed to read ledger")
        .expect("Block not found")
}

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
