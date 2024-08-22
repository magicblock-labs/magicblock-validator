use std::collections::HashMap;

use sleipnir_accounts_db::transaction_results::TransactionExecutionDetails;
use sleipnir_bank::bank::Bank;
use solana_sdk::{
    signature::Signature,
    transaction::{SanitizedTransaction, Transaction},
};

#[derive(Default, Debug)]
pub struct TransactionsProcessorProcessResult {
    pub transactions:
        HashMap<Signature, (SanitizedTransaction, TransactionExecutionDetails)>,
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
