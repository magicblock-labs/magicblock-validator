// NOTE: adapted from ledger/src/blockstore_processor.rs

use std::sync::Arc;

use sleipnir_bank::{bank::Bank, transaction_batch::TransactionBatch};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::transaction::Result;

use crate::transaction_status::TransactionStatusSender;

pub struct TransactionBatchWithIndexes<'a, 'b> {
    pub batch: TransactionBatch<'a, 'b>,
    pub transaction_indexes: Vec<usize>,
}

pub fn execute_batch(
    batch: &TransactionBatchWithIndexes,
    bank: &Arc<Bank>,
    transaction_status_sender: Option<&TransactionStatusSender>,
    timings: &mut ExecuteTimings,
    log_messages_bytes_limit: Option<usize>,
) -> Result<()> {
    Ok(())
}
