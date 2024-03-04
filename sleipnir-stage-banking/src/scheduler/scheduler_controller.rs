// NOTE: from core/src/banking_stage/transaction_scheduler/scheduler_controller.rs
// with lots of pieces removed that we don't need
use crate::scheduler::transaction_state::SanitizedTransactionTTL;
use crate::{
    consts::TOTAL_BUFFERED_PACKETS,
    consumer::Consumer,
    metrics::{ConsumeWorkerMetrics, SchedulerCountMetrics, SchedulerTimingMetrics},
};

use sleipnir_messaging::immutable_deserialized_packet::ImmutableDeserializedPacket;
use sleipnir_messaging::packet_deserializer::PacketDeserializer;

use solana_program_runtime::compute_budget_processor::process_compute_budget_instructions;
use solana_sdk::feature_set::include_loaded_accounts_data_size_in_fee_calculation;
use std::{sync::Arc, time::Duration};

use crossbeam_channel::RecvTimeoutError;
use sleipnir_bank::bank::Bank;

use solana_cost_model::cost_model::CostModel;
use solana_measure::measure_us;
use solana_sdk::{
    clock::MAX_PROCESSING_AGE, fee::FeeBudgetLimits, saturating_add_assign,
    transaction::SanitizedTransaction,
};
use solana_svm::transaction_error_metrics::TransactionErrorMetrics;

use super::{
    prio_graph_scheduler::PrioGraphScheduler, scheduler_error::SchedulerError,
    transaction_id_generator::TransactionIdGenerator,
    transaction_state_container::TransactionStateContainer,
};

// Removed:
// - decision_maker: DecisionMaker,
// Commented out the parts that we still need to implement

const DEFAULT_CHUNK_SIZE: usize = 128;

/// Controls packet and transaction flow into scheduler, and scheduling execution.
pub(crate) struct SchedulerController {
    /// Packet/Transaction ingress.
    packet_receiver: PacketDeserializer,

    // changed from BankForks since we only have one
    bank: Arc<Bank>,

    /// Generates unique IDs for incoming transactions.
    transaction_id_generator: TransactionIdGenerator,

    /// Container for transaction state.
    /// Shared resource between `packet_receiver` and `scheduler`.
    container: TransactionStateContainer,

    /// State for scheduling and communicating with worker threads.
    scheduler: PrioGraphScheduler,

    /// Metrics tracking counts on transactions in different states.
    count_metrics: SchedulerCountMetrics,

    /// Metrics tracking time spent in different code sections.
    timing_metrics: SchedulerTimingMetrics,

    /// Metric report handles for the worker threads.
    worker_metrics: Vec<Arc<ConsumeWorkerMetrics>>,

    /// Sleipir specific chunk size override, defaults to 128 as used in Solana Validator
    chunk_size: usize,
}

impl SchedulerController {
    pub fn new(
        packet_deserializer: PacketDeserializer,
        bank: Arc<Bank>,
        scheduler: PrioGraphScheduler,
        worker_metrics: Vec<Arc<ConsumeWorkerMetrics>>,
        chunk_size: Option<usize>,
    ) -> Self {
        Self {
            packet_receiver: packet_deserializer,
            bank,
            transaction_id_generator: TransactionIdGenerator::default(),
            container: TransactionStateContainer::with_capacity(TOTAL_BUFFERED_PACKETS),
            scheduler,
            count_metrics: SchedulerCountMetrics::default(),
            timing_metrics: SchedulerTimingMetrics::default(),
            worker_metrics,
            chunk_size: chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE),
        }
    }

    pub fn run(mut self) -> Result<(), SchedulerError> {
        loop {
            self.process_transactions()?;
            self.receive_completed()?;
            if !self.receive_and_buffer_packets() {
                break;
            }
            // Report metrics only if there is data.
            // Reset intervals when appropriate, regardless of report.
            let should_report = self.count_metrics.has_data();
            self.count_metrics
                .update_priority_stats(self.container.get_min_max_priority());
            self.count_metrics.maybe_report_and_reset(should_report);
            self.timing_metrics.maybe_report_and_reset(should_report);
            self.worker_metrics
                .iter()
                .for_each(|metrics| metrics.maybe_report_and_reset());
        }

        Ok(())
    }

    /// Process packets
    /// NOTE: original based how to process the packet on the DecisionMaker which we don't have, we
    /// only include the BufferedPacketsDecision::Consumed variant here
    fn process_transactions(&mut self) -> Result<(), SchedulerError> {
        let working_bank = &self.bank;
        let (scheduling_summary, schedule_time_us) = measure_us!(self.scheduler.schedule(
            &mut self.container,
            |txs, results| { Self::pre_graph_filter(txs, results, working_bank) },
            |_| true // no pre-lock filter for now
        )?);
        saturating_add_assign!(
            self.count_metrics.num_scheduled,
            scheduling_summary.num_scheduled
        );
        saturating_add_assign!(
            self.count_metrics.num_unschedulable,
            scheduling_summary.num_unschedulable
        );
        saturating_add_assign!(
            self.count_metrics.num_schedule_filtered_out,
            scheduling_summary.num_filtered_out
        );
        saturating_add_assign!(
            self.timing_metrics.schedule_filter_time_us,
            scheduling_summary.filter_time_us
        );
        saturating_add_assign!(self.timing_metrics.schedule_time_us, schedule_time_us);

        Ok(())
    }

    fn pre_graph_filter(transactions: &[&SanitizedTransaction], results: &mut [bool], bank: &Bank) {
        let lock_results = vec![Ok(()); transactions.len()];
        let mut error_counters = TransactionErrorMetrics::default();
        let check_results = bank.check_transactions(
            transactions,
            &lock_results,
            MAX_PROCESSING_AGE,
            &mut error_counters,
        );

        let fee_check_results: Vec<_> = check_results
            .into_iter()
            .zip(transactions)
            .map(|((result, _nonce, _lamports), tx)| {
                result?; // if there's already error do nothing
                Consumer::check_fee_payer_unlocked(bank, tx.message(), &mut error_counters)
            })
            .collect();

        for (fee_check_result, result) in fee_check_results.into_iter().zip(results.iter_mut()) {
            *result = fee_check_result.is_ok();
        }
    }

    /// Clears the transaction state container.
    /// This only clears pending transactions, and does **not** clear in-flight transactions.
    fn clear_container(&mut self) {
        while let Some(id) = self.container.pop() {
            self.container.remove_by_id(&id.id);
            saturating_add_assign!(self.count_metrics.num_dropped_on_clear, 1);
        }
    }

    /// Clean unprocessable transactions from the queue. These will be transactions that are
    /// expired, already processed, or are no longer sanitizable.
    /// This only clears pending transactions, and does **not** clear in-flight transactions.
    fn clean_queue(&mut self) {
        // Clean up any transactions that have already been processed, are too old, or do not have
        // valid nonce accounts.
        const MAX_TRANSACTION_CHECKS: usize = 10_000;
        let mut transaction_ids = Vec::with_capacity(MAX_TRANSACTION_CHECKS);

        while let Some(id) = self.container.pop() {
            transaction_ids.push(id);
        }

        // NOTE: this gets working_bank from bank_forks in original
        let bank = &self.bank;

        let chunk_size = self.chunk_size;
        let mut error_counters = TransactionErrorMetrics::default();

        for chunk in transaction_ids.chunks(chunk_size) {
            let lock_results = vec![Ok(()); chunk.len()];
            let sanitized_txs: Vec<_> = chunk
                .iter()
                .map(|id| {
                    &self
                        .container
                        .get_transaction_ttl(&id.id)
                        .expect("transaction must exist")
                        .transaction
                })
                .collect();

            let check_results = bank.check_transactions(
                &sanitized_txs,
                &lock_results,
                MAX_PROCESSING_AGE,
                &mut error_counters,
            );

            for ((result, _nonce, _lamports), id) in check_results.into_iter().zip(chunk.iter()) {
                if result.is_err() {
                    saturating_add_assign!(self.count_metrics.num_dropped_on_age_and_status, 1);
                    self.container.remove_by_id(&id.id);
                }
            }
        }
    }

    /// Receives completed transactions from the workers and updates metrics.
    fn receive_completed(&mut self) -> Result<(), SchedulerError> {
        let ((num_transactions, num_retryable), receive_completed_time_us) =
            measure_us!(self.scheduler.receive_completed(&mut self.container)?);
        saturating_add_assign!(self.count_metrics.num_finished, num_transactions);
        saturating_add_assign!(self.count_metrics.num_retryable, num_retryable);
        saturating_add_assign!(
            self.timing_metrics.receive_completed_time_us,
            receive_completed_time_us
        );
        Ok(())
    }

    /// Returns whether the packet receiver is still connected.
    // NOTE: only kept code path based on `BufferedPacketsDecision::Consume`
    fn receive_and_buffer_packets(&mut self) -> bool {
        let remaining_queue_capacity = self.container.remaining_queue_capacity();

        const MAX_PACKET_RECEIVE_TIME: Duration = Duration::from_millis(100);
        let (recv_timeout, should_buffer) = {
            (
                if self.container.is_empty() {
                    MAX_PACKET_RECEIVE_TIME
                } else {
                    Duration::ZERO
                },
                true,
            )
        };

        let (received_packet_results, receive_time_us) = measure_us!(self
            .packet_receiver
            .receive_packets(recv_timeout, remaining_queue_capacity));
        saturating_add_assign!(self.timing_metrics.receive_time_us, receive_time_us);

        match received_packet_results {
            Ok(receive_packet_results) => {
                let num_received_packets = receive_packet_results.deserialized_packets.len();
                saturating_add_assign!(self.count_metrics.num_received, num_received_packets);
                if should_buffer {
                    let (_, buffer_time_us) = measure_us!(
                        self.buffer_packets(receive_packet_results.deserialized_packets)
                    );
                    saturating_add_assign!(self.timing_metrics.buffer_time_us, buffer_time_us);
                } else {
                    saturating_add_assign!(
                        self.count_metrics.num_dropped_on_receive,
                        num_received_packets
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return false,
        }

        true
    }

    fn buffer_packets(&mut self, packets: Vec<ImmutableDeserializedPacket>) {
        // Sanitize packets, generate IDs, and insert into the container.
        // NOTE: this gets working_bank from bank_forks in original
        let bank = &self.bank;
        let last_slot_in_epoch = bank.epoch_schedule().get_last_slot_in_epoch(bank.epoch());
        let transaction_account_lock_limit = bank.get_transaction_account_lock_limit();
        let feature_set = &bank.feature_set;

        let chunk_size: usize = self.chunk_size;
        // NOTE: this used const CHUNK_SIZE to create an fixed-size array
        // let lock_results: [_; CHUNK_SIZE] = core::array::from_fn(|_| Ok(()));
        let lock_results = vec![Ok(()); chunk_size];
        let mut error_counts = TransactionErrorMetrics::default();
        for chunk in packets.chunks(chunk_size) {
            let mut post_sanitization_count: usize = 0;
            let (transactions, fee_budget_limits_vec): (Vec<_>, Vec<_>) = chunk
                .iter()
                .filter_map(|packet| packet.build_sanitized_transaction(feature_set, bank.as_ref()))
                .inspect(|_| saturating_add_assign!(post_sanitization_count, 1))
                .filter(|tx| {
                    SanitizedTransaction::validate_account_locks(
                        tx.message(),
                        transaction_account_lock_limit,
                    )
                    .is_ok()
                })
                .filter_map(|tx| {
                    process_compute_budget_instructions(tx.message().program_instructions_iter())
                        .map(|compute_budget| (tx, compute_budget.into()))
                        .ok()
                })
                .unzip();

            let check_results = bank.check_transactions(
                &transactions,
                &lock_results[..transactions.len()],
                MAX_PROCESSING_AGE,
                &mut error_counts,
            );
            let post_lock_validation_count = transactions.len();

            let mut post_transaction_check_count: usize = 0;
            for ((transaction, fee_budget_limits), _) in transactions
                .into_iter()
                .zip(fee_budget_limits_vec)
                .zip(check_results)
                .filter(|(_, check_result)| check_result.0.is_ok())
            {
                saturating_add_assign!(post_transaction_check_count, 1);
                let transaction_id = self.transaction_id_generator.next();

                let (priority, cost) =
                    Self::calculate_priority_and_cost(&transaction, &fee_budget_limits, bank);
                let transaction_ttl = SanitizedTransactionTTL {
                    transaction,
                    max_age_slot: last_slot_in_epoch,
                };

                if self.container.insert_new_transaction(
                    transaction_id,
                    transaction_ttl,
                    priority,
                    cost,
                ) {
                    saturating_add_assign!(self.count_metrics.num_dropped_on_capacity, 1);
                }
                saturating_add_assign!(self.count_metrics.num_buffered, 1);
            }

            // Update metrics for transactions that were dropped.
            let num_dropped_on_sanitization = chunk.len().saturating_sub(post_sanitization_count);
            let num_dropped_on_lock_validation =
                post_sanitization_count.saturating_sub(post_lock_validation_count);
            let num_dropped_on_transaction_checks =
                post_lock_validation_count.saturating_sub(post_transaction_check_count);

            saturating_add_assign!(
                self.count_metrics.num_dropped_on_sanitization,
                num_dropped_on_sanitization
            );
            saturating_add_assign!(
                self.count_metrics.num_dropped_on_validate_locks,
                num_dropped_on_lock_validation
            );
            saturating_add_assign!(
                self.count_metrics.num_dropped_on_receive_transaction_checks,
                num_dropped_on_transaction_checks
            );
        }
    }

    /// Calculate priority and cost for a transaction:
    ///
    /// Cost is calculated through the `CostModel`,
    /// and priority is calculated through a formula here that attempts to sell
    /// blockspace to the highest bidder.
    ///
    /// The priority is calculated as:
    /// P = R / (1 + C)
    /// where P is the priority, R is the reward,
    /// and C is the cost towards block-limits.
    ///
    /// Current minimum costs are on the order of several hundred,
    /// so the denominator is effectively C, and the +1 is simply
    /// to avoid any division by zero due to a bug - these costs
    /// are calculated by the cost-model and are not direct
    /// from user input. They should never be zero.
    /// Any difference in the prioritization is negligible for
    /// the current transaction costs.
    fn calculate_priority_and_cost(
        transaction: &SanitizedTransaction,
        fee_budget_limits: &FeeBudgetLimits,
        bank: &Bank,
    ) -> (u64, u64) {
        let cost = CostModel::calculate_cost(transaction, &bank.feature_set).sum();
        let fee = bank.fee_structure.calculate_fee(
            transaction.message(),
            5_000, // this just needs to be non-zero
            fee_budget_limits,
            bank.feature_set
                .is_active(&include_loaded_accounts_data_size_in_fee_calculation::id()),
        );

        // We need a multiplier here to avoid rounding down too aggressively.
        // For many transactions, the cost will be greater than the fees in terms of raw lamports.
        // For the purposes of calculating prioritization, we multiply the fees by a large number so that
        // the cost is a small fraction.
        // An offset of 1 is used in the denominator to explicitly avoid division by zero.
        const MULTIPLIER: u64 = 1_000_000;
        (
            fee.saturating_mul(MULTIPLIER)
                .saturating_div(cost.saturating_add(1)),
            cost,
        )
    }
}
