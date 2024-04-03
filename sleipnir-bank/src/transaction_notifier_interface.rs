use {
    solana_sdk::{
        clock::Slot, signature::Signature, transaction::SanitizedTransaction,
    },
    solana_transaction_status::TransactionStatusMeta,
    std::fmt::Debug,
    std::sync::Arc,
};

pub trait TransactionNotifier: Debug {
    fn notify_transaction(
        &self,
        slot: Slot,
        transaction_slot_index: usize,
        signature: &Signature,
        transaction_status_meta: &TransactionStatusMeta,
        transaction: &SanitizedTransaction,
    );
}

pub type TransactionNotifierArc = Arc<dyn TransactionNotifier + Sync + Send>;
