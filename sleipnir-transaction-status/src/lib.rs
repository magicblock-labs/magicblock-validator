use std::sync::Arc;

use crossbeam_channel::Sender;
use log::trace;
use magicblock_accounts_db::transaction_results::{
    TransactionExecutionDetails, TransactionExecutionResult,
};
use magicblock_bank::{
    bank::Bank, transaction_results::TransactionBalancesSet,
};
use solana_sdk::{
    clock::Slot, rent_debits::RentDebits, transaction::SanitizedTransaction,
};
use solana_transaction_status::token_balances::TransactionTokenBalancesSet;
pub use solana_transaction_status::*;

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum TransactionStatusMessage {
    Batch(TransactionStatusBatch),
    Freeze(Slot),
}

// NOTE: copied from ledger/src/blockstore_processor.rs:1819
pub struct TransactionStatusBatch {
    pub bank: Arc<Bank>,
    pub transactions: Vec<SanitizedTransaction>,
    pub execution_results: Vec<Option<TransactionExecutionDetails>>,
    pub balances: TransactionBalancesSet,
    pub token_balances: TransactionTokenBalancesSet,
    pub rent_debits: Vec<RentDebits>,
    pub transaction_slot_indexes: Vec<usize>,
}

impl std::fmt::Debug for TransactionStatusBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransactionStatusBatch")
            .field("transactions", &self.transactions)
            .field("execution_results", &self.execution_results)
            .field("balances", &self.balances)
            .field("rent_debits", &self.rent_debits)
            .field("transaction_slot_indexes", &self.transaction_slot_indexes)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct TransactionStatusSender {
    pub sender: Sender<TransactionStatusMessage>,
}

impl TransactionStatusSender {
    #[allow(clippy::too_many_arguments)]
    pub fn send_transaction_status_batch(
        &self,
        bank: &Arc<Bank>,
        transactions: Vec<SanitizedTransaction>,
        execution_results: Vec<TransactionExecutionResult>,
        balances: TransactionBalancesSet,
        token_balances: TransactionTokenBalancesSet,
        rent_debits: Vec<RentDebits>,
        transaction_slot_indexes: Vec<usize>,
    ) {
        let slot = bank.slot();

        if let Err(e) = self.sender.send(TransactionStatusMessage::Batch(
            TransactionStatusBatch {
                bank: bank.clone(),
                transactions,
                execution_results: execution_results
                    .into_iter()
                    .map(|result| match result {
                        TransactionExecutionResult::Executed {
                            details,
                            ..
                        } => Some(details),
                        TransactionExecutionResult::NotExecuted(_) => None,
                    })
                    .collect(),
                balances,
                token_balances,
                rent_debits,
                transaction_slot_indexes,
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
