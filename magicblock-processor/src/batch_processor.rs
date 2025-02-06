// NOTE: adapted from ledger/src/blockstore_processor.rs

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use log::debug;
use magicblock_bank::{bank::Bank, transaction_batch::TransactionBatch};
use magicblock_transaction_status::{
    token_balances::TransactionTokenBalancesSet, TransactionStatusSender,
};
use rayon::prelude::*;
use solana_measure::{measure::Measure, measure_us};
use solana_sdk::{pubkey::Pubkey, transaction::Result};
use solana_svm::{
    transaction_commit_result::TransactionCommitResultExtensions,
    transaction_processor::ExecutionRecordingConfig,
};
use solana_timings::{ExecuteTimingType, ExecuteTimings};

use crate::{
    metrics::{
        BatchExecutionTiming, ExecuteBatchesInternalMetrics,
        ThreadExecuteTimings,
    },
    token_balances::collect_token_balances,
    utils::{first_err, get_first_error, PAR_THREAD_POOL},
};

pub struct TransactionBatchWithIndexes<'a, 'b> {
    pub batch: TransactionBatch<'a, 'b>,
    pub transaction_indexes: Vec<usize>,
}

// -----------------
// Processing Batches
// -----------------
#[allow(unused)]
fn process_batches(
    bank: &Arc<Bank>,
    batches: &[TransactionBatchWithIndexes],
    transaction_status_sender: Option<&TransactionStatusSender>,
    batch_execution_timing: &mut BatchExecutionTiming,
    log_messages_bytes_limit: Option<usize>,
) -> Result<()> {
    // NOTE: left out code path for bank with its own scheduler
    debug!(
        "process_batches()/rebatch_and_execute_batches({} batches)",
        batches.len()
    );
    rebatch_and_execute_batches(
        bank,
        batches,
        transaction_status_sender,
        batch_execution_timing,
        log_messages_bytes_limit,
    )
}

fn rebatch_and_execute_batches(
    bank: &Arc<Bank>,
    batches: &[TransactionBatchWithIndexes],
    transaction_status_sender: Option<&TransactionStatusSender>,
    timing: &mut BatchExecutionTiming,
    log_messages_bytes_limit: Option<usize>,
) -> Result<()> {
    if batches.is_empty() {
        return Ok(());
    }

    // NOTE: left out transaction cost tracking and rebatching considering cost
    // as a result this doesn't do anything except accumulate timing metrics

    let execute_batches_internal_metrics = execute_batches_internal(
        bank,
        batches,
        transaction_status_sender,
        log_messages_bytes_limit,
    )?;

    timing.accumulate(execute_batches_internal_metrics);
    Ok(())
}

// -----------------
// Execution
// -----------------
fn execute_batches_internal(
    bank: &Arc<Bank>,
    batches: &[TransactionBatchWithIndexes],
    transaction_status_sender: Option<&TransactionStatusSender>,
    log_messages_bytes_limit: Option<usize>,
) -> Result<ExecuteBatchesInternalMetrics> {
    assert!(!batches.is_empty());
    let execution_timings_per_thread: Mutex<
        HashMap<usize, ThreadExecuteTimings>,
    > = Mutex::new(HashMap::new());

    let mut execute_batches_elapsed = Measure::start("execute_batches_elapsed");
    let results: Vec<Result<()>> = PAR_THREAD_POOL.install(|| {
        batches
            .into_par_iter()
            .map(|transaction_batch| {
                let transaction_count =
                    transaction_batch.batch.sanitized_transactions().len()
                        as u64;
                let mut timings = ExecuteTimings::default();
                let (result, execute_batches_us) = measure_us!(execute_batch(
                    transaction_batch,
                    bank,
                    transaction_status_sender,
                    &mut timings,
                    log_messages_bytes_limit,
                ));

                let thread_index =
                    PAR_THREAD_POOL.current_thread_index().unwrap();
                execution_timings_per_thread
                    .lock()
                    .unwrap()
                    .entry(thread_index)
                    .and_modify(|thread_execution_time| {
                        let ThreadExecuteTimings {
                            total_thread_us,
                            total_transactions_executed,
                            execute_timings: total_thread_execute_timings,
                        } = thread_execution_time;
                        *total_thread_us += execute_batches_us;
                        *total_transactions_executed += transaction_count;
                        total_thread_execute_timings.saturating_add_in_place(
                            ExecuteTimingType::TotalBatchesLen,
                            1,
                        );
                        total_thread_execute_timings.accumulate(&timings);
                    })
                    .or_insert(ThreadExecuteTimings {
                        total_thread_us: execute_batches_us,
                        total_transactions_executed: transaction_count,
                        execute_timings: timings,
                    });
                result
            })
            .collect()
    });
    execute_batches_elapsed.stop();

    first_err(&results)?;

    Ok(ExecuteBatchesInternalMetrics {
        execution_timings_per_thread: execution_timings_per_thread
            .into_inner()
            .unwrap(),
        total_batches_len: batches.len() as u64,
        execute_batches_us: execute_batches_elapsed.as_us(),
    })
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

    let (commit_results, balances) =
        batch.bank().load_execute_and_commit_transactions(
            batch,
            transaction_status_sender.is_some(),
            ExecutionRecordingConfig::new_single_setting(
                transaction_status_sender.is_some(),
            ),
            timings,
            log_messages_bytes_limit,
        );

    let first_err = get_first_error(batch, &commit_results);

    if let Some(transaction_status_sender) = transaction_status_sender {
        let transactions = batch.sanitized_transactions().to_vec();
        let post_token_balances = if record_token_balances {
            collect_token_balances(bank, batch, &mut mint_decimals)
        } else {
            vec![]
        };

        let token_balances = TransactionTokenBalancesSet::new(
            pre_token_balances,
            post_token_balances,
        );

        transaction_status_sender.send_transaction_status_batch(
            bank.slot(),
            transactions,
            commit_results,
            balances,
            token_balances,
            transaction_indexes.to_vec(),
        );
    }

    first_err.map(|(result, _)| result).unwrap_or(Ok(()))
}
