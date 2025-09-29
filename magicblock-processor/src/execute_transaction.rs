use std::sync::Arc;

use lazy_static::lazy_static;
use magicblock_accounts_db::StWLock;
use magicblock_bank::bank::Bank;
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::{
    signature::Signature,
    transaction::{Result, SanitizedTransaction, Transaction},
};
use tokio::sync::Semaphore;

use crate::batch_processor::{execute_batch, TransactionBatchWithIndexes};

// NOTE: these don't exactly belong in the accounts crate
//       they should go into a dedicated crate that also has access to
//       magicblock_bank, magicblock_processor and magicblock_transaction_status
pub async fn execute_legacy_transaction(
    tx: Transaction,
    bank: &Arc<Bank>,
    transaction_status_sender: Option<&TransactionStatusSender>,
) -> Result<Signature> {
    let sanitized_tx = SanitizedTransaction::try_from_legacy_transaction(
        tx,
        &Default::default(),
    )?;
    execute_sanitized_transaction(sanitized_tx, bank, transaction_status_sender)
        .await
}

lazy_static! {
    pub static ref TRANSACTION_INDEX_LOCK: StWLock = StWLock::default();
}
lazy_static! {
    pub static ref TXN_SERIALIZER: Arc<Semaphore> = Arc::new(Semaphore::new(1));
}

pub async fn execute_sanitized_transaction(
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
    // Work tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/181
    let _execution_guard = TXN_SERIALIZER
        .acquire()
        .await
        .expect("semaphore has been closed");

    let batch = bank.prepare_sanitized_batch(txs);

    let batch_with_indexes = TransactionBatchWithIndexes {
        batch,
        // TODO: figure out how to properly derive transaction_indexes (index within the slot)
        // - This is important for the ledger history of each slot
        // - tracked: https://github.com/magicblock-labs/magicblock-validator/issues/201
        //
        // copied from agave/ledger/benches/blockstore_processor.rs:147
        transaction_indexes: (0..txs.len()).collect(),
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
