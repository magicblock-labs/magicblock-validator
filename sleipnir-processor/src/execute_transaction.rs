use lazy_static::lazy_static;
use std::sync::{Arc, Mutex};

use sleipnir_bank::bank::Bank;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{
    signature::Signature,
    transaction::{Result, SanitizedTransaction, Transaction},
};

use crate::batch_processor::{execute_batch, TransactionBatchWithIndexes};

// NOTE: these don't exactly belong in the accounts crate
//       they should go into a dedicated crate that also has access to
//       sleipnir_bank, sleipnir_processor and sleipnir_transaction_status
pub fn execute_legacy_transaction(
    tx: Transaction,
    bank: &Arc<Bank>,
    transaction_status_sender: Option<&TransactionStatusSender>,
) -> Result<Signature> {
    let sanitized_tx = SanitizedTransaction::try_from_legacy_transaction(tx)?;
    execute_sanitized_transaction(sanitized_tx, bank, transaction_status_sender)
}

lazy_static! {
    static ref TRANSACTION_MUTEX: Mutex<()> = Mutex::new(());
}

pub fn execute_sanitized_transaction(
    sanitized_tx: SanitizedTransaction,
    bank: &Arc<Bank>,
    transaction_status_sender: Option<&TransactionStatusSender>,
) -> Result<Signature> {
    let signature = *sanitized_tx.signature();
    let txs = &[sanitized_tx];

    // Ensure that only one transaction is processed at a time even if it is initiated from
    // multiple threads.
    // TODO: This is a temporary solution until we have a transaction executor which schedules
    // transactions to be executed in parallel without account lock conflicts.
    // If we choose this as a long term solution we need to lock simulations/preflight with the
    // same mutex once we enable them again
    let _transaction_lock = TRANSACTION_MUTEX.lock().unwrap();

    let batch = bank.prepare_sanitized_batch(txs);

    let batch_with_indexes = TransactionBatchWithIndexes {
        batch,
        // TODO(thlorenz): figure out how to properly derive transaction_indexes
        transaction_indexes: txs
            .iter()
            .enumerate()
            .map(|(idx, _)| idx)
            .collect(),
    };
    let mut timings = Default::default();
    execute_batch(
        &batch_with_indexes,
        bank,
        transaction_status_sender,
        &mut timings,
        None,
    )?;
    Ok(signature)
}
