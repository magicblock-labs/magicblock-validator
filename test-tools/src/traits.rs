use std::collections::HashMap;

use magicblock_bank::bank::Bank;
use solana_sdk::{
    signature::Signature,
    transaction::{SanitizedTransaction, Transaction},
};
use solana_svm::transaction_commit_result::CommittedTransaction;

#[derive(Default, Debug)]
pub struct TransactionsProcessorProcessResult {
    pub transactions:
        HashMap<Signature, (SanitizedTransaction, CommittedTransaction)>,
}

impl TransactionsProcessorProcessResult {
    #[must_use]
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub trait TransactionsProcessor {
    fn process(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<TransactionsProcessorProcessResult, String>;

    fn process_sanitized(
        &self,
        transactions: Vec<SanitizedTransaction>,
    ) -> Result<TransactionsProcessorProcessResult, String>;

    fn bank(&self) -> &Bank;
}
