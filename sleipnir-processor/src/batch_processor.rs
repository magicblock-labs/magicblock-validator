// NOTE: adapted from ledger/src/blockstore_processor.rs

use std::{collections::HashMap, sync::Arc};

use sleipnir_bank::{
    bank::{Bank, TransactionExecutionRecordingOpts},
    transaction_batch::TransactionBatch,
};
use solana_accounts_db::transaction_results::TransactionResults;
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{clock::MAX_PROCESSING_AGE, pubkey::Pubkey, transaction::Result};
use solana_transaction_status::token_balances::TransactionTokenBalancesSet;

use crate::{
    token_balances::collect_token_balances, transaction_status::TransactionStatusSender,
    utils::get_first_error,
};

pub struct TransactionBatchWithIndexes<'a, 'b> {
    pub batch: TransactionBatch<'a, 'b>,
    pub transaction_indexes: Vec<usize>,
}

#[allow(unused)]
pub fn execute_batch(
    batch: &TransactionBatchWithIndexes,
    bank: &Arc<Bank>,
    transaction_status_sender: Option<&TransactionStatusSender>,
    timings: &mut ExecuteTimings,
    log_messages_bytes_limit: Option<usize>,
) -> Result<()> {
    // 1. Record current balances
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

    // 2. Execute transactions in batch
    let recording_opts = TransactionExecutionRecordingOpts {
        enable_cpi_recording: transaction_status_sender.is_some(),
        enable_log_recording: transaction_status_sender.is_some(),
        enable_return_data_recording: transaction_status_sender.is_some(),
    };
    let collect_balances = transaction_status_sender.is_some();
    let (tx_results, balances) = batch.bank().load_execute_and_commit_transactions(
        batch,
        MAX_PROCESSING_AGE,
        collect_balances,
        recording_opts,
        timings,
        log_messages_bytes_limit,
    );

    // NOTE: left out find_and_send_votes

    // 3. Send results if sender is provided
    let TransactionResults {
        fee_collection_results,
        execution_results,
        rent_debits,
        ..
    } = tx_results;

    if let Some(transaction_status_sender) = transaction_status_sender {
        let transactions = batch.sanitized_transactions().to_vec();
        let post_token_balances = if record_token_balances {
            collect_token_balances(bank, batch, &mut mint_decimals)
        } else {
            vec![]
        };

        let token_balances =
            TransactionTokenBalancesSet::new(pre_token_balances, post_token_balances);

        transaction_status_sender.send_transaction_status_batch(
            bank.clone(),
            transactions,
            execution_results,
            balances,
            token_balances,
            rent_debits,
            transaction_indexes.to_vec(),
        );
    }

    // NOTE: left out prioritization_fee_cache.update and related executed_transactions aggregation

    // 4. Return first error
    let first_err = get_first_error(batch, fee_collection_results);
    first_err.map(|(result, _)| result).unwrap_or(Ok(()))
}
