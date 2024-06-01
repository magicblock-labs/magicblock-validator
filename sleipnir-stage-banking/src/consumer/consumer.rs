// NOTE: Adapted from core/src/banking_stage/consumer.rs
use std::{collections::HashMap, sync::Arc};

use itertools::Itertools;
use log::trace;
use sleipnir_bank::{
    bank::{Bank, TransactionExecutionRecordingOpts},
    get_compute_budget_details::GetComputeBudgetDetails,
    transaction_batch::TransactionBatch,
    transaction_results::LoadAndExecuteTransactionsOutput,
};
use sleipnir_tokens::token_balances::collect_token_balances;
use sleipnir_transaction_status::TransactionTokenBalance;
use solana_measure::measure_us;
use solana_program_runtime::{
    compute_budget_processor::process_compute_budget_instructions,
    timings::ExecuteTimings,
};
use solana_sdk::{
    clock::MAX_PROCESSING_AGE,
    feature_set,
    message::{AddressLoader, SanitizedMessage},
    pubkey::Pubkey,
    transaction::{SanitizedTransaction, TransactionError},
};
use solana_svm::{
    account_loader::validate_fee_payer,
    transaction_error_metrics::TransactionErrorMetrics,
    transaction_processor::TransactionProcessingCallback,
};

use crate::{
    committer::{CommitTransactionDetails, Committer},
    metrics::LeaderExecuteAndCommitTimings,
    qos_service::QosService,
    results::{
        ExecuteAndCommitTransactionsOutput, ProcessTransactionBatchOutput,
    },
};

/// Consumer will create chunks of transactions from buffer with up to this size.
pub const TARGET_NUM_TRANSACTIONS_PER_BATCH: usize = 64;

#[derive(Default)]
pub(crate) struct PreBalanceInfo {
    pub native: Vec<Vec<u64>>,
    pub token: Vec<Vec<TransactionTokenBalance>>,
    pub mint_decimals: HashMap<Pubkey, u8>,
}

// Removed the following
// - transaction_recorder: TransactionRecorder (poh)
// - qos_service: QosService, (cost calcualation)

#[allow(dead_code)]
pub struct Consumer {
    committer: Committer,
    qos_service: QosService,
    log_messages_bytes_limit: Option<usize>,
}

#[allow(dead_code)]
impl Consumer {
    pub fn new(
        committer: Committer,
        qos_service: QosService,
        log_messages_bytes_limit: Option<usize>,
    ) -> Self {
        Self {
            committer,
            qos_service,
            log_messages_bytes_limit,
        }
    }

    pub fn check_fee_payer_unlocked(
        bank: &Bank,
        message: &SanitizedMessage,
        error_counters: &mut TransactionErrorMetrics,
    ) -> Result<(), TransactionError> {
        let fee_payer = message.fee_payer();
        let budget_limits = process_compute_budget_instructions(
            message.program_instructions_iter(),
        )?
        .into();
        let fee = bank.fee_structure.calculate_fee(
            message,
            bank.get_lamports_per_signature(),
            &budget_limits,
            bank.feature_set.is_active(
                &feature_set::include_loaded_accounts_data_size_in_fee_calculation::id(),
            ),
        );
        let mut fee_payer_account = bank
            .rc
            .accounts
            .accounts_db
            .load(fee_payer)
            .ok_or(TransactionError::AccountNotFound)?;

        validate_fee_payer(
            fee_payer,
            &mut fee_payer_account,
            0,
            error_counters,
            bank.get_rent_collector(),
            fee,
        )
    }

    pub(crate) fn process_and_record_aged_transactions(
        &self,
        bank: &Arc<Bank>,
        txs: &[SanitizedTransaction],
        max_slot_ages: &[u64],
    ) -> ProcessTransactionBatchOutput {
        // Need to filter out transactions since they were sanitized earlier.
        // This means that the transaction may cross and epoch boundary (not allowed),
        //  or account lookup tables may have been closed.
        let pre_results =
            txs.iter().zip(max_slot_ages).map(|(tx, max_slot_age)| {
                if *max_slot_age < bank.slot() {
                    // Attempt re-sanitization after epoch-cross.
                    // Re-sanitized transaction should be equal to the original transaction,
                    // but whether it will pass sanitization needs to be checked.
                    let resanitized_tx = bank.fully_verify_transaction(
                        tx.to_versioned_transaction(),
                    )?;
                    if resanitized_tx != *tx {
                        // Sanitization before/after epoch give different transaction data - do not execute.
                        return Err(TransactionError::ResanitizationNeeded);
                    }
                } else {
                    // Any transaction executed between sanitization time and now may have closed the lookup table(s).
                    // Above re-sanitization already loads addresses, so don't need to re-check in that case.
                    let lookup_tables =
                        tx.message().message_address_table_lookups();
                    if !lookup_tables.is_empty() {
                        bank.load_addresses(lookup_tables)?;
                    }
                }
                Ok(())
            });
        self.process_and_record_transactions_with_pre_results(
            bank,
            txs,
            0,
            pre_results,
        )
    }

    fn process_and_record_transactions_with_pre_results(
        &self,
        bank: &Arc<Bank>,
        txs: &[SanitizedTransaction],
        chunk_offset: usize,
        pre_results: impl Iterator<Item = Result<(), TransactionError>>,
    ) -> ProcessTransactionBatchOutput {
        let (
            (
                transaction_qos_cost_results,
                cost_model_throttled_transactions_count,
            ),
            cost_model_us,
        ) = measure_us!(self
            .qos_service
            .select_and_accumulate_transaction_costs(bank, txs, pre_results));

        // Only lock accounts for those transactions are selected for the block;
        // Once accounts are locked, other threads cannot encode transactions that will modify the
        // same account state
        let (batch, lock_us) = measure_us!(bank
            .prepare_sanitized_batch_with_results(
                txs,
                transaction_qos_cost_results.iter().map(|r| match r {
                    Ok(_cost) => Ok(()),
                    Err(err) => Err(err.clone()),
                })
            ));
        // retryable_txs includes AccountInUse, WouldExceedMaxBlockCostLimit
        // WouldExceedMaxAccountCostLimit, WouldExceedMaxVoteCostLimit
        // and WouldExceedMaxAccountDataCostLimit
        let mut execute_and_commit_transactions_output =
            self.execute_and_commit_transactions_locked(bank, &batch);

        // Once the accounts are new transactions can enter the pipeline to process them
        let (_, unlock_us) = measure_us!(drop(batch));

        let ExecuteAndCommitTransactionsOutput {
            ref mut retryable_transaction_indexes,
            ref execute_and_commit_timings,
            ref commit_transactions_result,
            ..
        } = execute_and_commit_transactions_output;

        // Costs of all transactions are added to the cost_tracker before processing.
        // To ensure accurate tracking of compute units, transactions that ultimately
        // were not included in the block should have their cost removed.
        QosService::remove_costs(
            transaction_qos_cost_results.iter(),
            Some(commit_transactions_result),
            bank,
        );

        // once feature `apply_cost_tracker_during_replay` is activated, leader shall no longer
        // adjust block with executed cost (a behavior more inline with bankless leader), it
        // should use requested, or default `compute_unit_limit` as transaction's execution cost.
        if !bank
            .feature_set
            .is_active(&feature_set::apply_cost_tracker_during_replay::id())
        {
            QosService::update_costs(
                transaction_qos_cost_results.iter(),
                Some(commit_transactions_result),
                bank,
            );
        }

        retryable_transaction_indexes
            .iter_mut()
            .for_each(|x| *x += chunk_offset);

        let (cu, us) = Self::accumulate_execute_units_and_time(
            &execute_and_commit_timings.execute_timings,
        );
        self.qos_service.accumulate_actual_execute_cu(cu);
        self.qos_service.accumulate_actual_execute_time(us);

        // reports qos service stats for this batch
        self.qos_service.report_metrics(bank.slot());

        trace!(
            "bank: {} lock: {}us unlock: {}us txs_len: {}",
            bank.slot(),
            lock_us,
            unlock_us,
            txs.len(),
        );

        ProcessTransactionBatchOutput {
            cost_model_throttled_transactions_count,
            cost_model_us,
            execute_and_commit_transactions_output,
        }
    }

    fn execute_and_commit_transactions_locked(
        &self,
        bank: &Arc<Bank>,
        batch: &TransactionBatch,
    ) -> ExecuteAndCommitTransactionsOutput {
        let transaction_status_sender_enabled =
            self.committer.transaction_status_sender_enabled();
        let mut execute_and_commit_timings =
            LeaderExecuteAndCommitTimings::default();

        let mut pre_balance_info = PreBalanceInfo::default();
        let (_, collect_balances_us) = measure_us!({
            // If the extra meta-data services are enabled for RPC, collect the
            // pre-balances for native and token programs.
            if transaction_status_sender_enabled {
                pre_balance_info.native = bank.collect_balances(batch);
                pre_balance_info.token = collect_token_balances(
                    bank,
                    batch,
                    &mut pre_balance_info.mint_decimals,
                )
            }
        });
        execute_and_commit_timings.collect_balances_us = collect_balances_us;

        let min_max = batch
            .sanitized_transactions()
            .iter()
            .filter_map(|transaction| {
                let round_compute_unit_price_enabled = false; // TODO get from working_bank.feature_set
                transaction
                    .get_compute_budget_details(
                        round_compute_unit_price_enabled,
                    )
                    .map(|details| details.compute_unit_price)
            })
            .minmax();
        let (min_prioritization_fees, max_prioritization_fees) =
            min_max.into_option().unwrap_or_default();

        let (load_and_execute_transactions_output, load_execute_us) =
            measure_us!(bank.load_and_execute_transactions(
                batch,
                MAX_PROCESSING_AGE,
                TransactionExecutionRecordingOpts::recording_all_if(
                    transaction_status_sender_enabled
                ),
                &mut execute_and_commit_timings.execute_timings,
                None, // account_overrides
                self.log_messages_bytes_limit
            ));
        execute_and_commit_timings.load_execute_us = load_execute_us;

        let LoadAndExecuteTransactionsOutput {
            mut loaded_transactions,
            execution_results,
            retryable_transaction_indexes,
            executed_transactions_count,
            executed_non_vote_transactions_count,
            executed_with_successful_result_count,
            signature_count,
            error_counters,
            ..
        } = load_and_execute_transactions_output;

        let transactions_attempted_execution_count = execution_results.len();

        // NOTE: omitted executed_transactions aggregation since we don't record transactions

        let (freeze_lock, freeze_lock_us) = measure_us!(bank.freeze_lock());
        execute_and_commit_timings.freeze_lock_us = freeze_lock_us;

        // In order to avoid a race condition, leaders must get the last
        // blockhash *before* recording transactions because recording
        // transactions will only succeed if the block max tick height hasn't
        // been reached yet. If they get the last blockhash *after* recording
        // transactions, the block max tick height could have already been
        // reached and the blockhash queue could have already been updated with
        // a new blockhash.
        let ((last_blockhash, lamports_per_signature), last_blockhash_us) =
            measure_us!(bank.last_blockhash_and_lamports_per_signature());
        execute_and_commit_timings.last_blockhash_us = last_blockhash_us;

        // NOTE: omitted RecordTransactionSummary via transaction_recorder.record_transactions
        // NOTE: also omitted returning on recorder_err

        // TODO: build committer.commit_transactions

        // Originally this was the result of record_transactions
        let starting_transaction_index = None;
        let (commit_time_us, commit_transaction_statuses) =
            if executed_transactions_count != 0 {
                self.committer.commit_transactions(
                    batch,
                    &mut loaded_transactions,
                    execution_results,
                    last_blockhash,
                    lamports_per_signature,
                    starting_transaction_index,
                    bank,
                    &mut pre_balance_info,
                    &mut execute_and_commit_timings,
                    signature_count,
                    executed_transactions_count,
                    executed_non_vote_transactions_count,
                    executed_with_successful_result_count,
                )
            } else {
                (
                    0,
                    vec![
                        CommitTransactionDetails::NotCommitted;
                        execution_results.len()
                    ],
                )
            };

        drop(freeze_lock);

        trace!(
            "bank: {} process_and_record_locked: {}us commit: {}us txs_len: {}",
            bank.slot(),
            load_execute_us,
            commit_time_us,
            batch.sanitized_transactions().len(),
        );

        trace!(
            "execute_and_commit_transactions_locked: {:?}",
            execute_and_commit_timings.execute_timings,
        );

        debug_assert_eq!(
            commit_transaction_statuses.len(),
            transactions_attempted_execution_count
        );

        ExecuteAndCommitTransactionsOutput {
            transactions_attempted_execution_count,
            executed_transactions_count,
            executed_with_successful_result_count,
            retryable_transaction_indexes,
            commit_transactions_result: commit_transaction_statuses,
            execute_and_commit_timings,
            error_counters,
            min_prioritization_fees,
            max_prioritization_fees,
        }
    }

    fn accumulate_execute_units_and_time(
        execute_timings: &ExecuteTimings,
    ) -> (u64, u64) {
        execute_timings.details.per_program_timings.values().fold(
            (0, 0),
            |(units, times), program_timings| {
                (
                    units.saturating_add(program_timings.accumulated_units),
                    times.saturating_add(program_timings.accumulated_us),
                )
            },
        )
    }
}
