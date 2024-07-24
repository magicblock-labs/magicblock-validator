// NOTE: from core/src/banking_stage/committer.rs

use std::sync::Arc;

use itertools::Itertools;
use sleipnir_accounts_db::{
    accounts::TransactionLoadResult,
    transaction_results::{TransactionExecutionResult, TransactionResults},
};
use sleipnir_bank::{
    bank::{Bank, CommitTransactionCounts},
    transaction_batch::TransactionBatch,
    transaction_results::TransactionBalancesSet,
};
use sleipnir_tokens::token_balances::collect_token_balances;
use sleipnir_transaction_status::{
    token_balances::TransactionTokenBalancesSet, TransactionStatusSender,
};
use solana_measure::measure_us;
use solana_sdk::{hash::Hash, saturating_add_assign};

use crate::{consumer::PreBalanceInfo, metrics::LeaderExecuteAndCommitTimings};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitTransactionDetails {
    Committed { compute_units: u64 },
    NotCommitted,
}

// NOTE: removed the following:
// - replay_vote_sender: ReplayVoteSender,
// - prioritization_fee_cache: Arc<PrioritizationFeeCache>,
#[derive(Clone)]
pub struct Committer {
    transaction_status_sender: Option<TransactionStatusSender>,
}

impl Committer {
    pub fn new(
        transaction_status_sender: Option<TransactionStatusSender>,
    ) -> Self {
        Self {
            transaction_status_sender,
        }
    }

    pub(super) fn transaction_status_sender_enabled(&self) -> bool {
        self.transaction_status_sender.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn commit_transactions(
        &self,
        batch: &TransactionBatch,
        loaded_transactions: &mut [TransactionLoadResult],
        execution_results: Vec<TransactionExecutionResult>,
        last_blockhash: Hash,
        lamports_per_signature: u64,
        starting_transaction_index: Option<usize>,
        bank: &Arc<Bank>,
        pre_balance_info: &mut PreBalanceInfo,
        execute_and_commit_timings: &mut LeaderExecuteAndCommitTimings,
        signature_count: u64,
        executed_transactions_count: usize,
        executed_non_vote_transactions_count: usize,
        executed_with_successful_result_count: usize,
    ) -> (u64, Vec<CommitTransactionDetails>) {
        // NOTE: omitted executed_transactions aggregation since we don't update prioritzation_fee_cache
        let (tx_results, commit_time_us) = measure_us!(bank
            .commit_transactions(
                batch.sanitized_transactions(),
                loaded_transactions,
                execution_results,
                last_blockhash,
                lamports_per_signature,
                CommitTransactionCounts {
                    committed_transactions_count: executed_transactions_count
                        as u64,
                    committed_non_vote_transactions_count:
                        executed_non_vote_transactions_count as u64,
                    committed_with_failure_result_count:
                        executed_transactions_count.saturating_sub(
                            executed_with_successful_result_count
                        ) as u64,
                    signature_count,
                },
                &mut execute_and_commit_timings.execute_timings,
            ));
        execute_and_commit_timings.commit_us = commit_time_us;

        let commit_transaction_statuses = tx_results
            .execution_results
            .iter()
            .map(|execution_result| match execution_result.details() {
                Some(details) => CommitTransactionDetails::Committed {
                    compute_units: details.executed_units,
                },
                None => CommitTransactionDetails::NotCommitted,
            })
            .collect::<Vec<_>>();

        let ((), find_and_send_votes_us) = measure_us!({
            // NOTE: removed bank_utils::find_and_send_votes
            self.collect_balances_and_send_status_batch(
                tx_results,
                bank,
                batch,
                pre_balance_info,
                starting_transaction_index,
            );
            // NOTE: removed self.prioritization_fee_cache.update
        });
        execute_and_commit_timings.find_and_send_votes_us =
            find_and_send_votes_us;
        (commit_time_us, commit_transaction_statuses)
    }

    fn collect_balances_and_send_status_batch(
        &self,
        tx_results: TransactionResults,
        bank: &Arc<Bank>,
        batch: &TransactionBatch,
        pre_balance_info: &mut PreBalanceInfo,
        starting_transaction_index: Option<usize>,
    ) {
        if let Some(transaction_status_sender) = &self.transaction_status_sender
        {
            let txs = batch.sanitized_transactions().to_vec();
            let post_balances = bank.collect_balances(batch);
            let post_token_balances = collect_token_balances(
                bank,
                batch,
                &mut pre_balance_info.mint_decimals,
            );
            let mut transaction_index =
                starting_transaction_index.unwrap_or_default();
            let batch_transaction_indexes: Vec<_> = tx_results
                .execution_results
                .iter()
                .map(|result| {
                    if result.was_executed() {
                        let this_transaction_index = transaction_index;
                        saturating_add_assign!(transaction_index, 1);
                        this_transaction_index
                    } else {
                        0
                    }
                })
                .collect();
            transaction_status_sender.send_transaction_status_batch(
                bank,
                txs,
                tx_results.execution_results,
                TransactionBalancesSet::new(
                    std::mem::take(&mut pre_balance_info.native),
                    post_balances,
                ),
                TransactionTokenBalancesSet::new(
                    std::mem::take(&mut pre_balance_info.token),
                    post_token_balances,
                ),
                tx_results.rent_debits,
                batch_transaction_indexes,
            );
        }
    }
}
