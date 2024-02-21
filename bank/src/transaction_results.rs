// NOTE: copied from bank.rs:294
use solana_accounts_db::{
    accounts::TransactionLoadResult, transaction_results::TransactionExecutionResult,
};
use solana_svm::transaction_error_metrics::TransactionErrorMetrics;

#[derive(Debug)]
pub struct LoadAndExecuteTransactionsOutput {
    pub loaded_transactions: Vec<TransactionLoadResult>,
    // Vector of results indicating whether a transaction was executed or could not
    // be executed. Note executed transactions can still have failed!
    pub execution_results: Vec<TransactionExecutionResult>,
    pub retryable_transaction_indexes: Vec<usize>,
    // Total number of transactions that were executed
    pub executed_transactions_count: usize,
    // Number of non-vote transactions that were executed
    pub executed_non_vote_transactions_count: usize,
    // Total number of the executed transactions that returned success/not
    // an error.
    pub executed_with_successful_result_count: usize,
    pub signature_count: u64,
    pub error_counters: TransactionErrorMetrics,
}
