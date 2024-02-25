// NOTE: adapted from ledger/src/blockstore_processor.rs

use std::{collections::HashMap, sync::Arc};

use sleipnir_bank::{bank::Bank, transaction_batch::TransactionBatch};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{pubkey::Pubkey, transaction::Result};

use crate::{token_balances::collect_token_balances, transaction_status::TransactionStatusSender};

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
    let TransactionBatchWithIndexes {
        batch,
        transaction_indexes,
    } = batch;
    let record_token_balances = transaction_status_sender.is_some();

    let mut mint_decimals: HashMap<Pubkey, u8> = HashMap::new();

    let pre_token_balances = if record_token_balances {
        collect_token_balances(bank, batch, &mut mint_decimals)
    } else {
        vec![]
    };

    Ok(())
}
