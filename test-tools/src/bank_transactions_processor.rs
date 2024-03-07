use std::{collections::HashMap, sync::Arc};

use sleipnir_bank::{
    bank::{Bank, TransactionExecutionRecordingOpts},
    genesis_utils::create_genesis_config,
};
use solana_accounts_db::transaction_results::TransactionResults;
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    clock::MAX_PROCESSING_AGE,
    transaction::{SanitizedTransaction, Transaction},
};

use crate::traits::{TransactionsProcessor, TransactionsProcessorProcessResult};

#[derive(Debug)]
pub struct BankTransactionsProcessor {
    pub bank: Arc<Bank>,
}

impl BankTransactionsProcessor {
    pub fn new(bank: Arc<Bank>) -> Self {
        Self { bank }
    }
}

impl Default for BankTransactionsProcessor {
    fn default() -> Self {
        let genesis_config = create_genesis_config(u64::MAX).genesis_config;
        let bank = Arc::new(Bank::new_for_tests(&genesis_config));
        Self::new(bank)
    }
}

impl TransactionsProcessor for BankTransactionsProcessor {
    fn process(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<TransactionsProcessorProcessResult, String> {
        let mut timings = ExecuteTimings::default();

        let transactions: Vec<SanitizedTransaction> = transactions
            .into_iter()
            .map(SanitizedTransaction::from_transaction_for_tests)
            .collect();

        let transaction_results = {
            let batch = self.bank.prepare_sanitized_batch(&transactions);

            let (transaction_results, _) = self.bank.load_execute_and_commit_transactions(
                &batch,
                MAX_PROCESSING_AGE,
                true,
                TransactionExecutionRecordingOpts::recording_logs(),
                &mut timings,
                None,
            );
            transaction_results
        };

        let TransactionResults {
            execution_results, ..
        } = transaction_results;
        // Assuming here that results come back in same order as the txs we sent in
        let transactions = transactions
            .into_iter()
            .zip(execution_results)
            .map(|(tx, res)| (*tx.signature(), (tx, res.details().cloned().unwrap())))
            .collect::<HashMap<_, _>>();

        Ok(TransactionsProcessorProcessResult { transactions })
    }

    fn bank(&self) -> &Bank {
        &self.bank
    }
}

#[cfg(test)]
mod tests {
    use sleipnir_bank::bank_dev_utils::transactions::create_funded_accounts;
    use solana_sdk::{native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, system_transaction};

    use crate::{diagnostics::log_exec_details, init_logger};

    use super::*;

    #[tokio::test]
    async fn test_system_transfer_enough_funds() {
        init_logger!();
        let tx_processor = BankTransactionsProcessor::default();
        let payers = create_funded_accounts(&tx_processor.bank, 1, Some(LAMPORTS_PER_SOL));
        let start_hash = tx_processor.bank.last_blockhash();
        let to = Pubkey::new_unique();
        let tx = system_transaction::transfer(&payers[0], &to, 890_880_000, start_hash);
        let result = tx_processor.process(vec![tx]).unwrap();

        assert_eq!(result.len(), 1);

        let (tx, _) = result.transactions.values().next().unwrap();
        assert_eq!(tx.signatures().len(), 1);
        assert_eq!(tx.message().account_keys().len(), 3);

        let status = tx_processor
            .bank
            .get_signature_status(&tx.signatures()[0])
            .unwrap();
        assert!(status.is_ok());
    }

    #[tokio::test]
    async fn test_system_transfer_not_enough_funds() {
        init_logger!();
        let tx_processor = BankTransactionsProcessor::default();
        let payers = create_funded_accounts(&tx_processor.bank, 1, Some(890_850_000));
        let start_hash = tx_processor.bank.last_blockhash();
        let to = Pubkey::new_unique();
        let tx = system_transaction::transfer(&payers[0], &to, 890_880_000, start_hash);
        let result = tx_processor.process(vec![tx]).unwrap();

        assert_eq!(result.len(), 1);

        let (tx, exec_details) = result.transactions.values().next().unwrap();
        assert_eq!(tx.signatures().len(), 1);
        assert_eq!(tx.message().account_keys().len(), 3);

        let status = tx_processor
            .bank
            .get_signature_status(&tx.signatures()[0])
            .unwrap();
        assert!(status.is_err());

        log_exec_details(exec_details);
    }
}
