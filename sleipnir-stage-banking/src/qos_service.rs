// NOTE: from core/src/banking_stage/qos_service.rs
// See: https://solana.blog/solana-network-upgrades-quic-stake-weight-qos-fee-markets/

//! Quality of service for block producer.
//! Provides logic and functions to allow a Leader to prioritize
//! how transactions are included in blocks, and optimize those blocks.
//!

use std::sync::atomic::{AtomicU64, Ordering};

use log::debug;
use sleipnir_bank::bank::Bank;
use solana_cost_model::{
    cost_model::CostModel, transaction_cost::TransactionCost,
};
use solana_measure::measure::Measure;
use solana_metrics::datapoint_info;
use solana_sdk::{
    clock::Slot,
    feature_set::FeatureSet,
    saturating_add_assign,
    transaction::{self, SanitizedTransaction, TransactionError},
};

use crate::{
    batch_transaction_details::BatchedTransactionDetails,
    committer::CommitTransactionDetails,
};

// QosService is local to each banking thread, each instance of QosService provides services to
// one banking thread.
// Banking thread calls `report_metrics(slot)` at end of `process_and_record_transaction()`, or any time
// it wants.
//
pub struct QosService {
    metrics: QosServiceMetrics,
}

impl QosService {
    pub fn new(id: u32) -> Self {
        Self {
            metrics: QosServiceMetrics::new(id),
        }
    }

    /// Calculate cost of transactions, if not already filtered out, determine which ones to
    /// include in the slot, and accumulate costs in the cost tracker.
    /// Returns a vector of results containing selected transaction costs, and the number of
    /// transactions that were *NOT* selected.
    pub fn select_and_accumulate_transaction_costs(
        &self,
        bank: &Bank,
        transactions: &[SanitizedTransaction],
        pre_results: impl Iterator<Item = transaction::Result<()>>,
    ) -> (Vec<transaction::Result<TransactionCost>>, usize) {
        let transaction_costs = self.compute_transaction_costs(
            &bank.feature_set,
            transactions.iter(),
            pre_results,
        );
        let (transactions_qos_cost_results, num_included) = self
            .select_transactions_per_cost(
                transactions.iter(),
                transaction_costs.into_iter(),
                bank,
            );
        self.accumulate_estimated_transaction_costs(
            &Self::accumulate_batched_transaction_costs(
                transactions_qos_cost_results.iter(),
            ),
        );
        let cost_model_throttled_transactions_count =
            transactions.len().saturating_sub(num_included);

        (
            transactions_qos_cost_results,
            cost_model_throttled_transactions_count,
        )
    }

    // invoke cost_model to calculate cost for the given list of transactions that have not
    // been filtered out already.
    fn compute_transaction_costs<'a>(
        &self,
        feature_set: &FeatureSet,
        transactions: impl Iterator<Item = &'a SanitizedTransaction>,
        pre_results: impl Iterator<Item = transaction::Result<()>>,
    ) -> Vec<transaction::Result<TransactionCost>> {
        let mut compute_cost_time = Measure::start("compute_cost_time");
        let txs_costs: Vec<_> = transactions
            .zip(pre_results)
            .map(|(tx, pre_result)| {
                pre_result.map(|()| CostModel::calculate_cost(tx, feature_set))
            })
            .collect();
        compute_cost_time.stop();
        self.metrics
            .stats
            .compute_cost_time
            .fetch_add(compute_cost_time.as_us(), Ordering::Relaxed);
        self.metrics
            .stats
            .compute_cost_count
            .fetch_add(txs_costs.len() as u64, Ordering::Relaxed);
        txs_costs
    }

    /// Given a list of transactions and their costs, this function returns a corresponding
    /// list of Results that indicate if a transaction is selected to be included in the current block,
    /// and a count of the number of transactions that would fit in the block
    fn select_transactions_per_cost<'a>(
        &self,
        transactions: impl Iterator<Item = &'a SanitizedTransaction>,
        transactions_costs: impl Iterator<
            Item = transaction::Result<TransactionCost>,
        >,
        bank: &Bank,
    ) -> (Vec<transaction::Result<TransactionCost>>, usize) {
        let mut cost_tracking_time = Measure::start("cost_tracking_time");
        let mut cost_tracker = bank.write_cost_tracker().unwrap();
        let mut num_included = 0;
        let select_results = transactions.zip(transactions_costs)
            .map(|(tx, cost)| {
                match cost {
                    Ok(cost) => {
                        match cost_tracker.try_add(&cost) {
                            Ok(current_block_cost) => {
                                debug!("slot {:?}, transaction {:?}, cost {:?}, fit into current block, current block cost {}", bank.slot(), tx, cost, current_block_cost);
                                self.metrics.stats.selected_txs_count.fetch_add(1, Ordering::Relaxed);
                                num_included += 1;
                                Ok(cost)
                            },
                            Err(e) => {
                                debug!("slot {:?}, transaction {:?}, cost {:?}, not fit into current block, '{:?}'", bank.slot(), tx, cost, e);
                                Err(TransactionError::from(e))
                            }
                        }
                    },
                    Err(e) => Err(e),
                }
            })
            .collect();

        cost_tracking_time.stop();
        self.metrics
            .stats
            .cost_tracking_time
            .fetch_add(cost_tracking_time.as_us(), Ordering::Relaxed);
        (select_results, num_included)
    }

    /// Updates the transaction costs for committed transactions. Does not handle removing costs
    /// for transactions that didn't get recorded or committed
    pub fn update_costs<'a>(
        transaction_cost_results: impl Iterator<
            Item = &'a transaction::Result<TransactionCost>,
        >,
        transaction_committed_status: Option<&Vec<CommitTransactionDetails>>,
        bank: &Bank,
    ) {
        if let Some(transaction_committed_status) = transaction_committed_status
        {
            Self::update_committed_transaction_costs(
                transaction_cost_results,
                transaction_committed_status,
                bank,
            )
        }
    }

    /// Removes transaction costs from the cost tracker if not committed or recorded
    pub fn remove_costs<'a>(
        transaction_cost_results: impl Iterator<
            Item = &'a transaction::Result<TransactionCost>,
        >,
        transaction_committed_status: Option<&Vec<CommitTransactionDetails>>,
        bank: &Bank,
    ) {
        match transaction_committed_status {
            Some(transaction_committed_status) => {
                Self::remove_uncommitted_transaction_costs(
                    transaction_cost_results,
                    transaction_committed_status,
                    bank,
                )
            }
            None => {
                Self::remove_transaction_costs(transaction_cost_results, bank)
            }
        }
    }

    fn remove_uncommitted_transaction_costs<'a>(
        transaction_cost_results: impl Iterator<
            Item = &'a transaction::Result<TransactionCost>,
        >,
        transaction_committed_status: &Vec<CommitTransactionDetails>,
        bank: &Bank,
    ) {
        let mut cost_tracker = bank.write_cost_tracker().unwrap();
        transaction_cost_results
            .zip(transaction_committed_status)
            .for_each(|(tx_cost, transaction_committed_details)| {
                // Only transactions that the qos service included have to be
                // checked for update
                if let Ok(tx_cost) = tx_cost {
                    if *transaction_committed_details
                        == CommitTransactionDetails::NotCommitted
                    {
                        cost_tracker.remove(tx_cost)
                    }
                }
            });
    }

    fn update_committed_transaction_costs<'a>(
        transaction_cost_results: impl Iterator<
            Item = &'a transaction::Result<TransactionCost>,
        >,
        transaction_committed_status: &Vec<CommitTransactionDetails>,
        bank: &Bank,
    ) {
        let mut cost_tracker = bank.write_cost_tracker().unwrap();
        transaction_cost_results
            .zip(transaction_committed_status)
            .for_each(|(tx_cost, transaction_committed_details)| {
                // Only transactions that the qos service included have to be
                // checked for update
                if let Ok(tx_cost) = tx_cost {
                    if let CommitTransactionDetails::Committed {
                        compute_units,
                    } = transaction_committed_details
                    {
                        cost_tracker
                            .update_execution_cost(tx_cost, *compute_units)
                    }
                }
            });
    }

    fn remove_transaction_costs<'a>(
        transaction_cost_results: impl Iterator<
            Item = &'a transaction::Result<TransactionCost>,
        >,
        bank: &Bank,
    ) {
        let mut cost_tracker = bank.write_cost_tracker().unwrap();
        transaction_cost_results.for_each(|tx_cost| {
            // Only transactions that the qos service included have to be
            // removed
            if let Ok(tx_cost) = tx_cost {
                cost_tracker.remove(tx_cost);
            }
        });
    }

    // metrics are reported by bank slot
    pub fn report_metrics(&self, slot: Slot) {
        self.metrics.report(slot);
    }

    fn accumulate_estimated_transaction_costs(
        &self,
        batched_transaction_details: &BatchedTransactionDetails,
    ) {
        self.metrics.stats.estimated_signature_cu.fetch_add(
            batched_transaction_details.costs.batched_signature_cost,
            Ordering::Relaxed,
        );
        self.metrics.stats.estimated_write_lock_cu.fetch_add(
            batched_transaction_details.costs.batched_write_lock_cost,
            Ordering::Relaxed,
        );
        self.metrics.stats.estimated_data_bytes_cu.fetch_add(
            batched_transaction_details.costs.batched_data_bytes_cost,
            Ordering::Relaxed,
        );
        self.metrics.stats.estimated_builtins_execute_cu.fetch_add(
            batched_transaction_details
                .costs
                .batched_builtins_execute_cost,
            Ordering::Relaxed,
        );
        self.metrics.stats.estimated_bpf_execute_cu.fetch_add(
            batched_transaction_details.costs.batched_bpf_execute_cost,
            Ordering::Relaxed,
        );

        self.metrics
            .errors
            .retried_txs_per_block_limit_count
            .fetch_add(
                batched_transaction_details
                    .errors
                    .batched_retried_txs_per_block_limit_count,
                Ordering::Relaxed,
            );
        self.metrics
            .errors
            .retried_txs_per_vote_limit_count
            .fetch_add(
                batched_transaction_details
                    .errors
                    .batched_retried_txs_per_vote_limit_count,
                Ordering::Relaxed,
            );
        self.metrics
            .errors
            .retried_txs_per_account_limit_count
            .fetch_add(
                batched_transaction_details
                    .errors
                    .batched_retried_txs_per_account_limit_count,
                Ordering::Relaxed,
            );
        self.metrics
            .errors
            .retried_txs_per_account_data_block_limit_count
            .fetch_add(
                batched_transaction_details
                    .errors
                    .batched_retried_txs_per_account_data_block_limit_count,
                Ordering::Relaxed,
            );
        self.metrics
            .errors
            .dropped_txs_per_account_data_total_limit_count
            .fetch_add(
                batched_transaction_details
                    .errors
                    .batched_dropped_txs_per_account_data_total_limit_count,
                Ordering::Relaxed,
            );
    }

    pub fn accumulate_actual_execute_cu(&self, units: u64) {
        self.metrics
            .stats
            .actual_bpf_execute_cu
            .fetch_add(units, Ordering::Relaxed);
    }

    pub fn accumulate_actual_execute_time(&self, micro_sec: u64) {
        self.metrics
            .stats
            .actual_execute_time_us
            .fetch_add(micro_sec, Ordering::Relaxed);
    }

    // rollup transaction cost details, eg signature_cost, write_lock_cost, data_bytes_cost and
    // execution_cost from the batch of transactions selected for block.
    fn accumulate_batched_transaction_costs<'a>(
        transactions_costs: impl Iterator<
            Item = &'a transaction::Result<TransactionCost>,
        >,
    ) -> BatchedTransactionDetails {
        let mut batched_transaction_details =
            BatchedTransactionDetails::default();
        transactions_costs.for_each(|cost| match cost {
            Ok(cost) => {
                saturating_add_assign!(
                    batched_transaction_details.costs.batched_signature_cost,
                    cost.signature_cost()
                );
                saturating_add_assign!(
                    batched_transaction_details.costs.batched_write_lock_cost,
                    cost.write_lock_cost()
                );
                saturating_add_assign!(
                    batched_transaction_details.costs.batched_data_bytes_cost,
                    cost.data_bytes_cost()
                );
                saturating_add_assign!(
                    batched_transaction_details
                        .costs
                        .batched_builtins_execute_cost,
                    cost.builtins_execution_cost()
                );
                saturating_add_assign!(
                    batched_transaction_details.costs.batched_bpf_execute_cost,
                    cost.bpf_execution_cost()
                );
            }
            Err(transaction_error) => match transaction_error {
                TransactionError::WouldExceedMaxBlockCostLimit => {
                    saturating_add_assign!(
                        batched_transaction_details
                            .errors
                            .batched_retried_txs_per_block_limit_count,
                        1
                    );
                }
                TransactionError::WouldExceedMaxVoteCostLimit => {
                    saturating_add_assign!(
                        batched_transaction_details
                            .errors
                            .batched_retried_txs_per_vote_limit_count,
                        1
                    );
                }
                TransactionError::WouldExceedMaxAccountCostLimit => {
                    saturating_add_assign!(
                        batched_transaction_details
                            .errors
                            .batched_retried_txs_per_account_limit_count,
                        1
                    );
                }
                TransactionError::WouldExceedAccountDataBlockLimit => {
                    saturating_add_assign!(
                        batched_transaction_details
                            .errors
                            .batched_retried_txs_per_account_data_block_limit_count,
                        1
                    );
                }
                TransactionError::WouldExceedAccountDataTotalLimit => {
                    saturating_add_assign!(
                        batched_transaction_details
                            .errors
                            .batched_dropped_txs_per_account_data_total_limit_count,
                        1
                    );
                }
                _ => {}
            },
        });
        batched_transaction_details
    }
}

#[derive(Debug, Default)]
struct QosServiceMetrics {
    /// banking_stage creates one QosService instance per working threads, that is uniquely
    /// identified by id. This field allows to categorize metrics for gossip votes, TPU votes
    /// and other transactions.
    id: u32,

    /// aggregate metrics per slot
    slot: AtomicU64,

    stats: QosServiceMetricsStats,
    errors: QosServiceMetricsErrors,
}

#[derive(Debug, Default)]
struct QosServiceMetricsStats {
    /// accumulated time in micro-sec spent in computing transaction cost. It is the main performance
    /// overhead introduced by cost_model
    compute_cost_time: AtomicU64,

    /// total nummber of transactions in the reporting period to be computed for theit cost. It is
    /// usually the number of sanitized transactions leader receives.
    compute_cost_count: AtomicU64,

    /// acumulated time in micro-sec spent in tracking each bank's cost. It is the second part of
    /// overhead introduced
    cost_tracking_time: AtomicU64,

    /// number of transactions to be included in blocks
    selected_txs_count: AtomicU64,

    /// accumulated estimated signature Compute Unites to be packed into block
    estimated_signature_cu: AtomicU64,

    /// accumulated estimated write locks Compute Units to be packed into block
    estimated_write_lock_cu: AtomicU64,

    /// accumulated estimated instructino data Compute Units to be packed into block
    estimated_data_bytes_cu: AtomicU64,

    /// accumulated estimated builtin programs Compute Units to be packed into block
    estimated_builtins_execute_cu: AtomicU64,

    /// accumulated estimated SBF program Compute Units to be packed into block
    estimated_bpf_execute_cu: AtomicU64,

    /// accumulated actual program Compute Units that have been packed into block
    actual_bpf_execute_cu: AtomicU64,

    /// accumulated actual program execute micro-sec that have been packed into block
    actual_execute_time_us: AtomicU64,
}

#[derive(Debug, Default)]
struct QosServiceMetricsErrors {
    /// number of transactions to be queued for retry due to their potential to breach block limit
    retried_txs_per_block_limit_count: AtomicU64,

    /// number of transactions to be queued for retry due to their potential to breach vote limit
    retried_txs_per_vote_limit_count: AtomicU64,

    /// number of transactions to be queued for retry due to their potential to breach writable
    /// account limit
    retried_txs_per_account_limit_count: AtomicU64,

    /// number of transactions to be queued for retry due to their potential to breach account data
    /// block limits
    retried_txs_per_account_data_block_limit_count: AtomicU64,

    /// number of transactions to be dropped due to their potential to breach account data total
    /// limits
    dropped_txs_per_account_data_total_limit_count: AtomicU64,
}

impl QosServiceMetrics {
    pub fn new(id: u32) -> Self {
        QosServiceMetrics {
            id,
            ..QosServiceMetrics::default()
        }
    }

    pub fn report(&self, bank_slot: Slot) {
        if bank_slot != self.slot.load(Ordering::Relaxed) {
            datapoint_info!(
                "qos-service-stats",
                ("id", self.id, i64),
                ("bank_slot", bank_slot, i64),
                (
                    "compute_cost_time",
                    self.stats.compute_cost_time.swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "compute_cost_count",
                    self.stats.compute_cost_count.swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "cost_tracking_time",
                    self.stats.cost_tracking_time.swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "selected_txs_count",
                    self.stats.selected_txs_count.swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "estimated_signature_cu",
                    self.stats
                        .estimated_signature_cu
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "estimated_write_lock_cu",
                    self.stats
                        .estimated_write_lock_cu
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "estimated_data_bytes_cu",
                    self.stats
                        .estimated_data_bytes_cu
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "estimated_builtins_execute_cu",
                    self.stats
                        .estimated_builtins_execute_cu
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "estimated_bpf_execute_cu",
                    self.stats
                        .estimated_bpf_execute_cu
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "actual_bpf_execute_cu",
                    self.stats.actual_bpf_execute_cu.swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "actual_execute_time_us",
                    self.stats
                        .actual_execute_time_us
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
            );
            datapoint_info!(
                "qos-service-errors",
                ("id", self.id, i64),
                ("bank_slot", bank_slot, i64),
                (
                    "retried_txs_per_block_limit_count",
                    self.errors
                        .retried_txs_per_block_limit_count
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "retried_txs_per_vote_limit_count",
                    self.errors
                        .retried_txs_per_vote_limit_count
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "retried_txs_per_account_limit_count",
                    self.errors
                        .retried_txs_per_account_limit_count
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "retried_txs_per_account_data_block_limit_count",
                    self.errors
                        .retried_txs_per_account_data_block_limit_count
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
                (
                    "dropped_txs_per_account_data_total_limit_count",
                    self.errors
                        .dropped_txs_per_account_data_total_limit_count
                        .swap(0, Ordering::Relaxed),
                    i64
                ),
            );
            self.slot.store(bank_slot, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use itertools::Itertools;
    use sleipnir_bank::genesis_utils::GenesisConfigInfo;
    use solana_cost_model::transaction_cost::UsageCostDetails;
    use solana_sdk::{
        hash::Hash,
        signature::{Keypair, Signer},
        system_transaction,
    };

    use super::*;
    use crate::genesis_utils::create_genesis_config;

    #[test]
    fn test_compute_transaction_costs() {
        solana_logger::setup();

        // make a vec of txs
        let keypair = Keypair::new();
        let transfer_tx = SanitizedTransaction::from_transaction_for_tests(
            system_transaction::transfer(
                &keypair,
                &keypair.pubkey(),
                1,
                Hash::default(),
            ),
        );
        let txs = vec![transfer_tx.clone(), transfer_tx];

        let qos_service = QosService::new(1);
        let txs_costs = qos_service.compute_transaction_costs(
            &FeatureSet::all_enabled(),
            txs.iter(),
            std::iter::repeat(Ok(())),
        );

        // verify the size of txs_costs and its contents
        assert_eq!(txs_costs.len(), txs.len());
        txs_costs
            .iter()
            .enumerate()
            .map(|(index, cost)| {
                assert_eq!(
                    cost.as_ref().unwrap().sum(),
                    CostModel::calculate_cost(
                        &txs[index],
                        &FeatureSet::all_enabled()
                    )
                    .sum()
                );
            })
            .collect_vec();
    }

    // TODO(thlorenz): this always comes back with num_selected = 1
    // and err results
    #[test]
    #[ignore]
    fn test_select_transactions_per_cost() {
        solana_logger::setup();
        let GenesisConfigInfo { genesis_config, .. } =
            create_genesis_config(10);
        let bank = Arc::new(Bank::new_for_tests(&genesis_config, None, None));

        let keypair = Keypair::new();
        let transfer_tx = SanitizedTransaction::from_transaction_for_tests(
            system_transaction::transfer(
                &keypair,
                &keypair.pubkey(),
                1,
                Hash::default(),
            ),
        );
        let transfer_tx_cost =
            CostModel::calculate_cost(&transfer_tx, &FeatureSet::all_enabled())
                .sum();

        // make a vec of txs
        let txs = vec![transfer_tx.clone(), transfer_tx];

        let qos_service = QosService::new(1);
        let txs_costs = qos_service.compute_transaction_costs(
            &FeatureSet::all_enabled(),
            txs.iter(),
            std::iter::repeat(Ok(())),
        );

        // set cost tracker limit to fit 1 transfer tx
        let cost_limit = transfer_tx_cost;
        bank.write_cost_tracker()
            .unwrap()
            .set_limits(cost_limit, cost_limit, cost_limit);
        let (results, num_selected) = qos_service.select_transactions_per_cost(
            txs.iter(),
            txs_costs.into_iter(),
            &bank,
        );
        assert_eq!(num_selected, 2);

        // verify that first transfer tx and first vote are allowed
        assert_eq!(results.len(), txs.len());
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());
        assert!(results[2].is_err());
        assert!(results[3].is_err());
    }

    #[test]
    fn test_update_and_remove_transaction_costs_committed() {
        solana_logger::setup();
        let GenesisConfigInfo { genesis_config, .. } =
            create_genesis_config(10);
        let bank = Arc::new(Bank::new_for_tests(&genesis_config, None, None));

        // make some transfer transactions
        // calculate their costs, apply to cost_tracker
        let transaction_count = 5;
        let keypair = Keypair::new();
        let transfer_tx = SanitizedTransaction::from_transaction_for_tests(
            system_transaction::transfer(
                &keypair,
                &keypair.pubkey(),
                1,
                Hash::default(),
            ),
        );
        let txs: Vec<SanitizedTransaction> = (0..transaction_count)
            .map(|_| transfer_tx.clone())
            .collect();
        let execute_units_adjustment = 10u64;

        // assert all tx_costs should be applied to cost_tracker if all execution_results are all committed
        {
            let qos_service = QosService::new(1);
            let txs_costs = qos_service.compute_transaction_costs(
                &FeatureSet::all_enabled(),
                txs.iter(),
                std::iter::repeat(Ok(())),
            );
            let total_txs_cost: u64 = txs_costs
                .iter()
                .map(|cost| cost.as_ref().unwrap().sum())
                .sum();
            let (qos_cost_results, _num_included) = qos_service
                .select_transactions_per_cost(
                    txs.iter(),
                    txs_costs.into_iter(),
                    &bank,
                );
            assert_eq!(
                total_txs_cost,
                bank.read_cost_tracker().unwrap().block_cost()
            );
            // all transactions are committed with actual units more than estimated
            let commited_status: Vec<CommitTransactionDetails> =
                qos_cost_results
                    .iter()
                    .map(|tx_cost| CommitTransactionDetails::Committed {
                        compute_units: tx_cost
                            .as_ref()
                            .unwrap()
                            .bpf_execution_cost()
                            + execute_units_adjustment,
                    })
                    .collect();
            let final_txs_cost =
                total_txs_cost + execute_units_adjustment * transaction_count;

            // All transactions are committed, no costs should be removed
            QosService::remove_costs(
                qos_cost_results.iter(),
                Some(&commited_status),
                &bank,
            );
            assert_eq!(
                total_txs_cost,
                bank.read_cost_tracker().unwrap().block_cost()
            );
            assert_eq!(
                transaction_count,
                bank.read_cost_tracker().unwrap().transaction_count()
            );

            QosService::update_costs(
                qos_cost_results.iter(),
                Some(&commited_status),
                &bank,
            );
            assert_eq!(
                final_txs_cost,
                bank.read_cost_tracker().unwrap().block_cost()
            );
            assert_eq!(
                transaction_count,
                bank.read_cost_tracker().unwrap().transaction_count()
            );
        }
    }

    #[test]
    fn test_update_and_remove_transaction_costs_not_committed() {
        solana_logger::setup();
        let GenesisConfigInfo { genesis_config, .. } =
            create_genesis_config(10);
        let bank = Arc::new(Bank::new_for_tests(&genesis_config, None, None));

        // make some transfer transactions
        // calculate their costs, apply to cost_tracker
        let transaction_count = 5;
        let keypair = Keypair::new();
        let transfer_tx = SanitizedTransaction::from_transaction_for_tests(
            system_transaction::transfer(
                &keypair,
                &keypair.pubkey(),
                1,
                Hash::default(),
            ),
        );
        let txs: Vec<SanitizedTransaction> = (0..transaction_count)
            .map(|_| transfer_tx.clone())
            .collect();

        // assert all tx_costs should be removed from cost_tracker if all execution_results are all Not Committed
        {
            let qos_service = QosService::new(1);
            let txs_costs = qos_service.compute_transaction_costs(
                &FeatureSet::all_enabled(),
                txs.iter(),
                std::iter::repeat(Ok(())),
            );
            let total_txs_cost: u64 = txs_costs
                .iter()
                .map(|cost| cost.as_ref().unwrap().sum())
                .sum();
            let (qos_cost_results, _num_included) = qos_service
                .select_transactions_per_cost(
                    txs.iter(),
                    txs_costs.into_iter(),
                    &bank,
                );
            assert_eq!(
                total_txs_cost,
                bank.read_cost_tracker().unwrap().block_cost()
            );

            // update costs doesn't impact non-committed
            QosService::update_costs(qos_cost_results.iter(), None, &bank);
            assert_eq!(
                total_txs_cost,
                bank.read_cost_tracker().unwrap().block_cost()
            );
            assert_eq!(
                transaction_count,
                bank.read_cost_tracker().unwrap().transaction_count()
            );

            QosService::remove_costs(qos_cost_results.iter(), None, &bank);
            assert_eq!(0, bank.read_cost_tracker().unwrap().block_cost());
            assert_eq!(
                0,
                bank.read_cost_tracker().unwrap().transaction_count()
            );
        }
    }

    #[test]
    fn test_update_and_remove_transaction_costs_mixed_execution() {
        solana_logger::setup();
        let GenesisConfigInfo { genesis_config, .. } =
            create_genesis_config(10);
        let bank = Arc::new(Bank::new_for_tests(&genesis_config, None, None));

        // make some transfer transactions
        // calculate their costs, apply to cost_tracker
        let transaction_count = 5;
        let keypair = Keypair::new();
        let transfer_tx = SanitizedTransaction::from_transaction_for_tests(
            system_transaction::transfer(
                &keypair,
                &keypair.pubkey(),
                1,
                Hash::default(),
            ),
        );
        let txs: Vec<SanitizedTransaction> = (0..transaction_count)
            .map(|_| transfer_tx.clone())
            .collect();
        let execute_units_adjustment = 10u64;

        // assert only commited tx_costs are applied cost_tracker
        {
            let qos_service = QosService::new(1);
            let txs_costs = qos_service.compute_transaction_costs(
                &FeatureSet::all_enabled(),
                txs.iter(),
                std::iter::repeat(Ok(())),
            );
            let total_txs_cost: u64 = txs_costs
                .iter()
                .map(|cost| cost.as_ref().unwrap().sum())
                .sum();
            let (qos_cost_results, _num_included) = qos_service
                .select_transactions_per_cost(
                    txs.iter(),
                    txs_costs.into_iter(),
                    &bank,
                );
            assert_eq!(
                total_txs_cost,
                bank.read_cost_tracker().unwrap().block_cost()
            );
            // Half of transactions are not committed, the rest with cost adjustment
            let commited_status: Vec<CommitTransactionDetails> =
                qos_cost_results
                    .iter()
                    .enumerate()
                    .map(|(n, tx_cost)| {
                        if n % 2 == 0 {
                            CommitTransactionDetails::NotCommitted
                        } else {
                            CommitTransactionDetails::Committed {
                                compute_units: tx_cost
                                    .as_ref()
                                    .unwrap()
                                    .bpf_execution_cost()
                                    + execute_units_adjustment,
                            }
                        }
                    })
                    .collect();

            QosService::remove_costs(
                qos_cost_results.iter(),
                Some(&commited_status),
                &bank,
            );
            QosService::update_costs(
                qos_cost_results.iter(),
                Some(&commited_status),
                &bank,
            );

            // assert the final block cost
            let mut expected_final_txs_count = 0u64;
            let mut expected_final_block_cost = 0u64;
            qos_cost_results.iter().enumerate().for_each(|(n, cost)| {
                if n % 2 != 0 {
                    expected_final_txs_count += 1;
                    expected_final_block_cost +=
                        cost.as_ref().unwrap().sum() + execute_units_adjustment;
                }
            });
            assert_eq!(
                expected_final_block_cost,
                bank.read_cost_tracker().unwrap().block_cost()
            );
            assert_eq!(
                expected_final_txs_count,
                bank.read_cost_tracker().unwrap().transaction_count()
            );
        }
    }

    #[test]
    fn test_accumulate_batched_transaction_costs() {
        let signature_cost = 1;
        let write_lock_cost = 2;
        let data_bytes_cost = 3;
        let builtins_execution_cost = 4;
        let bpf_execution_cost = 10;
        let num_txs = 4;

        let tx_cost_results: Vec<_> = (0..num_txs)
            .map(|n| {
                if n % 2 == 0 {
                    Ok(TransactionCost::Transaction(UsageCostDetails {
                        signature_cost,
                        write_lock_cost,
                        data_bytes_cost,
                        builtins_execution_cost,
                        bpf_execution_cost,
                        ..UsageCostDetails::default()
                    }))
                } else {
                    Err(TransactionError::WouldExceedMaxBlockCostLimit)
                }
            })
            .collect();
        // should only accumulate half of the costs that are OK
        let expected_signatures = signature_cost * (num_txs / 2);
        let expected_write_locks = write_lock_cost * (num_txs / 2);
        let expected_data_bytes = data_bytes_cost * (num_txs / 2);
        let expected_builtins_execution_costs =
            builtins_execution_cost * (num_txs / 2);
        let expected_bpf_execution_costs = bpf_execution_cost * (num_txs / 2);
        let batched_transaction_details =
            QosService::accumulate_batched_transaction_costs(
                tx_cost_results.iter(),
            );
        assert_eq!(
            expected_signatures,
            batched_transaction_details.costs.batched_signature_cost
        );
        assert_eq!(
            expected_write_locks,
            batched_transaction_details.costs.batched_write_lock_cost
        );
        assert_eq!(
            expected_data_bytes,
            batched_transaction_details.costs.batched_data_bytes_cost
        );
        assert_eq!(
            expected_builtins_execution_costs,
            batched_transaction_details
                .costs
                .batched_builtins_execute_cost
        );
        assert_eq!(
            expected_bpf_execution_costs,
            batched_transaction_details.costs.batched_bpf_execute_cost
        );
    }
}
