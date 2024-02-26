#![allow(dead_code)]
// NOTE: from core/src/banking_stage/consumer.rs:55
use solana_svm::transaction_error_metrics::TransactionErrorMetrics;

use crate::metrics::LeaderExecuteAndCommitTimings;

// NOTE: removed the following:
// - pub commit_transactions_result: Result<Vec<CommitTransactionDetails>, PohRecorderError>, (poh)

pub struct ExecuteAndCommitTransactionsOutput {
    // Total number of transactions that were passed as candidates for execution
    pub(crate) transactions_attempted_execution_count: usize,
    // The number of transactions of that were executed. See description of in `ProcessTransactionsSummary`
    // for possible outcomes of execution.
    pub(crate) executed_transactions_count: usize,
    // Total number of the executed transactions that returned success/not
    // an error.
    pub(crate) executed_with_successful_result_count: usize,
    // Transactions that either were not executed, or were executed and failed to be committed due
    // to the block ending.
    pub(crate) retryable_transaction_indexes: Vec<usize>,
    pub(crate) execute_and_commit_timings: LeaderExecuteAndCommitTimings,
    pub(crate) error_counters: TransactionErrorMetrics,
    pub(crate) min_prioritization_fees: u64,
    pub(crate) max_prioritization_fees: u64,
}
