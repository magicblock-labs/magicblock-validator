// NOTE: Adapted from core/src/banking_stage/consumer.rs
use crate::{committer::Committer, results::ExecuteAndCommitTransactionsOutput};
use sleipnir_bank::{bank::Bank, transaction_batch::TransactionBatch};
use solana_program_runtime::compute_budget_processor::process_compute_budget_instructions;
use solana_sdk::{feature_set, message::SanitizedMessage, transaction::TransactionError};
use solana_svm::{
    account_loader::validate_fee_payer, transaction_error_metrics::TransactionErrorMetrics,
    transaction_processor::TransactionProcessingCallback,
};
use std::sync::Arc;

/// Consumer will create chunks of transactions from buffer with up to this size.
pub const TARGET_NUM_TRANSACTIONS_PER_BATCH: usize = 64;

// Removed the following
// - transaction_recorder: TransactionRecorder (poh)
// - qos_service: QosService, (cost calcualation)

#[allow(dead_code)]
pub struct Consumer {
    committer: Committer,
    log_messages_bytes_limit: Option<usize>,
}

#[allow(dead_code)]
impl Consumer {
    pub fn new(committer: Committer, log_messages_bytes_limit: Option<usize>) -> Self {
        Self {
            committer,
            log_messages_bytes_limit,
        }
    }

    fn execute_and_commit_transactions_locked(
        &self,
        _bank: &Arc<Bank>,
        _batch: &TransactionBatch,
    ) -> ExecuteAndCommitTransactionsOutput {
        todo!()
    }

    pub fn check_fee_payer_unlocked(
        bank: &Bank,
        message: &SanitizedMessage,
        error_counters: &mut TransactionErrorMetrics,
    ) -> Result<(), TransactionError> {
        let fee_payer = message.fee_payer();
        let budget_limits =
            process_compute_budget_instructions(message.program_instructions_iter())?.into();
        let fee = bank.fee_structure.calculate_fee(
            message,
            bank.get_lamports_per_signature(),
            &budget_limits,
            bank.feature_set.is_active(
                &feature_set::include_loaded_accounts_data_size_in_fee_calculation::id(),
            ),
        );
        let (mut fee_payer_account, _slot) = bank
            .rc
            .accounts
            .accounts_db
            .load_with_fixed_root(&bank.ancestors, fee_payer)
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
}
