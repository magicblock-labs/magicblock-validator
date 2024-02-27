// From: core/src/banking_stage/consume_worker.rs :153

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use solana_metrics::datapoint_info;
use solana_sdk::timing::AtomicInterval;
use solana_svm::transaction_error_metrics::TransactionErrorMetrics;

use crate::results::{ExecuteAndCommitTransactionsOutput, ProcessTransactionBatchOutput};

use super::LeaderExecuteAndCommitTimings;

/// Metrics tracking number of packets processed by the consume worker.
/// These are atomic, and intended to be reported by the scheduling thread
/// since the consume worker thread is sleeping unless there is work to be
/// done.
pub(crate) struct ConsumeWorkerMetrics {
    id: u32,
    interval: AtomicInterval,
    pub(crate) has_data: AtomicBool,

    pub(crate) count_metrics: ConsumeWorkerCountMetrics,
    pub(crate) error_metrics: ConsumeWorkerTransactionErrorMetrics,
    pub(crate) timing_metrics: ConsumeWorkerTimingMetrics,
}

impl ConsumeWorkerMetrics {
    /// Report and reset metrics iff the interval has elapsed and the worker did some work.
    pub fn maybe_report_and_reset(&self) {
        const REPORT_INTERVAL_MS: u64 = 1000;
        if self.interval.should_update(REPORT_INTERVAL_MS)
            && self.has_data.swap(false, Ordering::Relaxed)
        {
            self.count_metrics.report_and_reset(self.id);
            self.timing_metrics.report_and_reset(self.id);
            self.error_metrics.report_and_reset(self.id);
        }
    }

    pub(crate) fn new(id: u32) -> Self {
        Self {
            id,
            interval: AtomicInterval::default(),
            has_data: AtomicBool::new(false),
            count_metrics: ConsumeWorkerCountMetrics::default(),
            error_metrics: ConsumeWorkerTransactionErrorMetrics::default(),
            timing_metrics: ConsumeWorkerTimingMetrics::default(),
        }
    }

    pub(crate) fn update_for_consume(
        &self,
        ProcessTransactionBatchOutput {
            cost_model_throttled_transactions_count,
            cost_model_us,
            execute_and_commit_transactions_output,
        }: &ProcessTransactionBatchOutput,
    ) {
        self.count_metrics
            .cost_model_throttled_transactions_count
            .fetch_add(*cost_model_throttled_transactions_count, Ordering::Relaxed);
        self.timing_metrics
            .cost_model_us
            .fetch_add(*cost_model_us, Ordering::Relaxed);
        self.update_on_execute_and_commit_transactions_output(
            execute_and_commit_transactions_output,
        );
    }

    fn update_on_execute_and_commit_transactions_output(
        &self,
        ExecuteAndCommitTransactionsOutput {
            transactions_attempted_execution_count,
            executed_transactions_count,
            executed_with_successful_result_count,
            retryable_transaction_indexes,
            execute_and_commit_timings,
            error_counters,
            min_prioritization_fees,
            max_prioritization_fees,
            ..
        }: &ExecuteAndCommitTransactionsOutput,
    ) {
        self.count_metrics
            .transactions_attempted_execution_count
            .fetch_add(*transactions_attempted_execution_count, Ordering::Relaxed);
        self.count_metrics
            .executed_transactions_count
            .fetch_add(*executed_transactions_count, Ordering::Relaxed);
        self.count_metrics
            .executed_with_successful_result_count
            .fetch_add(*executed_with_successful_result_count, Ordering::Relaxed);
        self.count_metrics
            .retryable_transaction_count
            .fetch_add(retryable_transaction_indexes.len(), Ordering::Relaxed);
        let min_prioritization_fees = self
            .count_metrics
            .min_prioritization_fees
            .fetch_min(*min_prioritization_fees, Ordering::Relaxed);
        let max_prioritization_fees = self
            .count_metrics
            .max_prioritization_fees
            .fetch_max(*max_prioritization_fees, Ordering::Relaxed);
        self.count_metrics
            .min_prioritization_fees
            .swap(min_prioritization_fees, Ordering::Relaxed);
        self.count_metrics
            .max_prioritization_fees
            .swap(max_prioritization_fees, Ordering::Relaxed);
        self.update_on_execute_and_commit_timings(execute_and_commit_timings);
        self.update_on_error_counters(error_counters);
    }

    fn update_on_execute_and_commit_timings(
        &self,
        LeaderExecuteAndCommitTimings {
            collect_balances_us,
            load_execute_us,
            freeze_lock_us,
            last_blockhash_us,
            record_us,
            commit_us,
            find_and_send_votes_us,
            ..
        }: &LeaderExecuteAndCommitTimings,
    ) {
        self.timing_metrics
            .collect_balances_us
            .fetch_add(*collect_balances_us, Ordering::Relaxed);
        self.timing_metrics
            .load_execute_us
            .fetch_add(*load_execute_us, Ordering::Relaxed);
        self.timing_metrics
            .freeze_lock_us
            .fetch_add(*freeze_lock_us, Ordering::Relaxed);
        self.timing_metrics
            .last_blockhash_us
            .fetch_add(*last_blockhash_us, Ordering::Relaxed);
        self.timing_metrics
            .record_us
            .fetch_add(*record_us, Ordering::Relaxed);
        self.timing_metrics
            .commit_us
            .fetch_add(*commit_us, Ordering::Relaxed);
        self.timing_metrics
            .find_and_send_votes_us
            .fetch_add(*find_and_send_votes_us, Ordering::Relaxed);
    }

    fn update_on_error_counters(
        &self,
        TransactionErrorMetrics {
            total,
            account_in_use,
            too_many_account_locks,
            account_loaded_twice,
            account_not_found,
            blockhash_not_found,
            blockhash_too_old,
            call_chain_too_deep,
            already_processed,
            instruction_error,
            insufficient_funds,
            invalid_account_for_fee,
            invalid_account_index,
            invalid_program_for_execution,
            not_allowed_during_cluster_maintenance,
            invalid_writable_account,
            invalid_rent_paying_account,
            would_exceed_max_block_cost_limit,
            would_exceed_max_account_cost_limit,
            would_exceed_max_vote_cost_limit,
            would_exceed_account_data_block_limit,
            max_loaded_accounts_data_size_exceeded,
            program_execution_temporarily_restricted,
        }: &TransactionErrorMetrics,
    ) {
        self.error_metrics
            .total
            .fetch_add(*total, Ordering::Relaxed);
        self.error_metrics
            .account_in_use
            .fetch_add(*account_in_use, Ordering::Relaxed);
        self.error_metrics
            .too_many_account_locks
            .fetch_add(*too_many_account_locks, Ordering::Relaxed);
        self.error_metrics
            .account_loaded_twice
            .fetch_add(*account_loaded_twice, Ordering::Relaxed);
        self.error_metrics
            .account_not_found
            .fetch_add(*account_not_found, Ordering::Relaxed);
        self.error_metrics
            .blockhash_not_found
            .fetch_add(*blockhash_not_found, Ordering::Relaxed);
        self.error_metrics
            .blockhash_too_old
            .fetch_add(*blockhash_too_old, Ordering::Relaxed);
        self.error_metrics
            .call_chain_too_deep
            .fetch_add(*call_chain_too_deep, Ordering::Relaxed);
        self.error_metrics
            .already_processed
            .fetch_add(*already_processed, Ordering::Relaxed);
        self.error_metrics
            .instruction_error
            .fetch_add(*instruction_error, Ordering::Relaxed);
        self.error_metrics
            .insufficient_funds
            .fetch_add(*insufficient_funds, Ordering::Relaxed);
        self.error_metrics
            .invalid_account_for_fee
            .fetch_add(*invalid_account_for_fee, Ordering::Relaxed);
        self.error_metrics
            .invalid_account_index
            .fetch_add(*invalid_account_index, Ordering::Relaxed);
        self.error_metrics
            .invalid_program_for_execution
            .fetch_add(*invalid_program_for_execution, Ordering::Relaxed);
        self.error_metrics
            .not_allowed_during_cluster_maintenance
            .fetch_add(*not_allowed_during_cluster_maintenance, Ordering::Relaxed);
        self.error_metrics
            .invalid_writable_account
            .fetch_add(*invalid_writable_account, Ordering::Relaxed);
        self.error_metrics
            .invalid_rent_paying_account
            .fetch_add(*invalid_rent_paying_account, Ordering::Relaxed);
        self.error_metrics
            .would_exceed_max_block_cost_limit
            .fetch_add(*would_exceed_max_block_cost_limit, Ordering::Relaxed);
        self.error_metrics
            .would_exceed_max_account_cost_limit
            .fetch_add(*would_exceed_max_account_cost_limit, Ordering::Relaxed);
        self.error_metrics
            .would_exceed_max_vote_cost_limit
            .fetch_add(*would_exceed_max_vote_cost_limit, Ordering::Relaxed);
        self.error_metrics
            .would_exceed_account_data_block_limit
            .fetch_add(*would_exceed_account_data_block_limit, Ordering::Relaxed);
        self.error_metrics
            .max_loaded_accounts_data_size_exceeded
            .fetch_add(*max_loaded_accounts_data_size_exceeded, Ordering::Relaxed);
        self.error_metrics
            .program_execution_temporarily_restricted
            .fetch_add(*program_execution_temporarily_restricted, Ordering::Relaxed);
    }
}

pub(crate) struct ConsumeWorkerCountMetrics {
    transactions_attempted_execution_count: AtomicUsize,
    executed_transactions_count: AtomicUsize,
    executed_with_successful_result_count: AtomicUsize,
    retryable_transaction_count: AtomicUsize,
    retryable_expired_bank_count: AtomicUsize,
    cost_model_throttled_transactions_count: AtomicUsize,
    min_prioritization_fees: AtomicU64,
    max_prioritization_fees: AtomicU64,
}

impl Default for ConsumeWorkerCountMetrics {
    fn default() -> Self {
        Self {
            transactions_attempted_execution_count: AtomicUsize::default(),
            executed_transactions_count: AtomicUsize::default(),
            executed_with_successful_result_count: AtomicUsize::default(),
            retryable_transaction_count: AtomicUsize::default(),
            retryable_expired_bank_count: AtomicUsize::default(),
            cost_model_throttled_transactions_count: AtomicUsize::default(),
            min_prioritization_fees: AtomicU64::new(u64::MAX),
            max_prioritization_fees: AtomicU64::default(),
        }
    }
}

impl ConsumeWorkerCountMetrics {
    fn report_and_reset(&self, id: u32) {
        datapoint_info!(
            "banking_stage_worker_counts",
            ("id", id, i64),
            (
                "transactions_attempted_execution_count",
                self.transactions_attempted_execution_count
                    .swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "executed_transactions_count",
                self.executed_transactions_count.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "executed_with_successful_result_count",
                self.executed_with_successful_result_count
                    .swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "retryable_transaction_count",
                self.retryable_transaction_count.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "retryable_expired_bank_count",
                self.retryable_expired_bank_count.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "cost_model_throttled_transactions_count",
                self.cost_model_throttled_transactions_count
                    .swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "min_prioritization_fees",
                self.min_prioritization_fees
                    .swap(u64::MAX, Ordering::Relaxed),
                i64
            ),
            (
                "max_prioritization_fees",
                self.max_prioritization_fees.swap(0, Ordering::Relaxed),
                i64
            ),
        );
    }
}

#[derive(Default)]
pub(crate) struct ConsumeWorkerTimingMetrics {
    cost_model_us: AtomicU64,
    collect_balances_us: AtomicU64,
    load_execute_us: AtomicU64,
    freeze_lock_us: AtomicU64,
    last_blockhash_us: AtomicU64,
    record_us: AtomicU64,
    commit_us: AtomicU64,
    find_and_send_votes_us: AtomicU64,
}

impl ConsumeWorkerTimingMetrics {
    fn report_and_reset(&self, id: u32) {
        datapoint_info!(
            "banking_stage_worker_timing",
            ("id", id, i64),
            (
                "cost_model_us",
                self.cost_model_us.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "collect_balances_us",
                self.collect_balances_us.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "load_execute_us",
                self.load_execute_us.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "freeze_lock_us",
                self.freeze_lock_us.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "last_blockhash_us",
                self.last_blockhash_us.swap(0, Ordering::Relaxed),
                i64
            ),
            ("record_us", self.record_us.swap(0, Ordering::Relaxed), i64),
            ("commit_us", self.commit_us.swap(0, Ordering::Relaxed), i64),
            (
                "find_and_send_votes_us",
                self.find_and_send_votes_us.swap(0, Ordering::Relaxed),
                i64
            ),
        );
    }
}

#[derive(Default)]
pub(crate) struct ConsumeWorkerTransactionErrorMetrics {
    total: AtomicUsize,
    account_in_use: AtomicUsize,
    too_many_account_locks: AtomicUsize,
    account_loaded_twice: AtomicUsize,
    account_not_found: AtomicUsize,
    blockhash_not_found: AtomicUsize,
    blockhash_too_old: AtomicUsize,
    call_chain_too_deep: AtomicUsize,
    already_processed: AtomicUsize,
    instruction_error: AtomicUsize,
    insufficient_funds: AtomicUsize,
    invalid_account_for_fee: AtomicUsize,
    invalid_account_index: AtomicUsize,
    invalid_program_for_execution: AtomicUsize,
    not_allowed_during_cluster_maintenance: AtomicUsize,
    invalid_writable_account: AtomicUsize,
    invalid_rent_paying_account: AtomicUsize,
    would_exceed_max_block_cost_limit: AtomicUsize,
    would_exceed_max_account_cost_limit: AtomicUsize,
    would_exceed_max_vote_cost_limit: AtomicUsize,
    would_exceed_account_data_block_limit: AtomicUsize,
    max_loaded_accounts_data_size_exceeded: AtomicUsize,
    program_execution_temporarily_restricted: AtomicUsize,
}

impl ConsumeWorkerTransactionErrorMetrics {
    fn report_and_reset(&self, id: u32) {
        datapoint_info!(
            "banking_stage_worker_error_metrics",
            ("id", id, i64),
            ("total", self.total.swap(0, Ordering::Relaxed), i64),
            (
                "account_in_use",
                self.account_in_use.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "too_many_account_locks",
                self.too_many_account_locks.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "account_loaded_twice",
                self.account_loaded_twice.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "account_not_found",
                self.account_not_found.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "blockhash_not_found",
                self.blockhash_not_found.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "blockhash_too_old",
                self.blockhash_too_old.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "call_chain_too_deep",
                self.call_chain_too_deep.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "already_processed",
                self.already_processed.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "instruction_error",
                self.instruction_error.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "insufficient_funds",
                self.insufficient_funds.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "invalid_account_for_fee",
                self.invalid_account_for_fee.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "invalid_account_index",
                self.invalid_account_index.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "invalid_program_for_execution",
                self.invalid_program_for_execution
                    .swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "not_allowed_during_cluster_maintenance",
                self.not_allowed_during_cluster_maintenance
                    .swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "invalid_writable_account",
                self.invalid_writable_account.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "invalid_rent_paying_account",
                self.invalid_rent_paying_account.swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "would_exceed_max_block_cost_limit",
                self.would_exceed_max_block_cost_limit
                    .swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "would_exceed_max_account_cost_limit",
                self.would_exceed_max_account_cost_limit
                    .swap(0, Ordering::Relaxed),
                i64
            ),
            (
                "would_exceed_max_vote_cost_limit",
                self.would_exceed_max_vote_cost_limit
                    .swap(0, Ordering::Relaxed),
                i64
            ),
        );
    }
}
