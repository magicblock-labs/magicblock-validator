use std::collections::HashMap;

use solana_sdk::{
    signature::Signature,
    transaction::{SanitizedTransaction, Transaction},
};

#[derive(Default)]
pub struct TransactionsProcessorProcessResult {
    pub transactions: HashMap<Signature, SanitizedTransaction>,
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
}
