use crossbeam_channel::Sender;
use log::trace;
use magicblock_bank::transaction_results::TransactionBalancesSet;
use solana_sdk::{clock::Slot, transaction::SanitizedTransaction};
use solana_svm::transaction_commit_result::TransactionCommitResult;
use solana_transaction_status::token_balances::TransactionTokenBalancesSet;
pub use solana_transaction_status::*;

#[allow(clippy::large_enum_variant)]
pub enum TransactionStatusMessage {
    Batch(TransactionStatusBatch),
    Freeze(Slot),
}

// NOTE: copied from ledger/src/blockstore_processor.rs:2206
pub struct TransactionStatusBatch {
    pub slot: Slot,
    pub transactions: Vec<SanitizedTransaction>,
    pub commit_results: Vec<TransactionCommitResult>,
    pub balances: TransactionBalancesSet,
    pub token_balances: TransactionTokenBalancesSet,
    pub transaction_indexes: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct TransactionStatusSender {
    pub sender: Sender<TransactionStatusMessage>,
}

impl TransactionStatusSender {
    #[allow(clippy::too_many_arguments)]
    pub fn send_transaction_status_batch(
        &self,
        slot: Slot,
        transactions: Vec<SanitizedTransaction>,
        commit_results: Vec<TransactionCommitResult>,
        balances: TransactionBalancesSet,
        token_balances: TransactionTokenBalancesSet,
        transaction_indexes: Vec<usize>,
    ) {
        if let Err(e) = self.sender.send(TransactionStatusMessage::Batch(
            TransactionStatusBatch {
                slot,
                transactions,
                commit_results,
                balances,
                token_balances,
                transaction_indexes,
            },
        )) {
            trace!(
                "Slot {} transaction_status send batch failed: {:?}",
                slot,
                e
            );
        }
    }
}
