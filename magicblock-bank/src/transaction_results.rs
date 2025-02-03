// NOTE: copied from bank.rs:294
use solana_svm::transaction_processing_result::TransactionProcessingResult;

#[derive(Debug)]
pub struct LoadAndExecuteTransactionsOutput {
    // Vector of results indicating whether a transaction was processed or could not
    // be processed. Note processed transactions can still have failed!
    pub processing_results: Vec<TransactionProcessingResult>,
    // Processed transaction counts used to update bank transaction counts and
    // for metrics reporting.
    pub processed_counts: ProcessedTransactionCounts,
}

#[derive(Debug, Default, PartialEq)]
pub struct ProcessedTransactionCounts {
    pub processed_transactions_count: u64,
    pub processed_non_vote_transactions_count: u64,
    pub processed_with_successful_result_count: u64,
    pub signature_count: u64,
}

#[derive(Debug)]
pub struct TransactionBalancesSet {
    pub pre_balances: TransactionBalances,
    pub post_balances: TransactionBalances,
}

impl TransactionBalancesSet {
    pub fn new(
        pre_balances: TransactionBalances,
        post_balances: TransactionBalances,
    ) -> Self {
        assert_eq!(pre_balances.len(), post_balances.len());
        Self {
            pre_balances,
            post_balances,
        }
    }
}
pub type TransactionBalances = Vec<Vec<u64>>;
