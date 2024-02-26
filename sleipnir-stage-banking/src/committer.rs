#![allow(dead_code)]
// NOTE: from core/src/banking_stage/committer.rs

use sleipnir_transaction_status::TransactionStatusSender;

// NOTE: removed the following:
// - replay_vote_sender: ReplayVoteSender,
// - prioritization_fee_cache: Arc<PrioritizationFeeCache>,
#[derive(Clone)]
pub struct Committer {
    transaction_status_sender: Option<TransactionStatusSender>,
}

impl Committer {
    pub fn new(transaction_status_sender: Option<TransactionStatusSender>) -> Self {
        Self {
            transaction_status_sender,
        }
    }

    pub(super) fn transaction_status_sender_enabled(&self) -> bool {
        self.transaction_status_sender.is_some()
    }
}
