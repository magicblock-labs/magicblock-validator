// NOTE: Adapted from core/src/banking_stage/consumer.rs
use crate::{committer::Committer, results::ExecuteAndCommitTransactionsOutput};
use sleipnir_bank::{bank::Bank, transaction_batch::TransactionBatch};
use std::sync::Arc;

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
}
