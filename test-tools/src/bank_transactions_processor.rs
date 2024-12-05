use std::{collections::HashMap, sync::Arc};

use magicblock_accounts_db::transaction_results::TransactionResults;
use magicblock_bank::{
    bank::{Bank, TransactionExecutionRecordingOpts},
    genesis_utils::create_genesis_config_with_leader_and_fees,
};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    pubkey::Pubkey,
    transaction::{SanitizedTransaction, Transaction},
};

use crate::{
    bank::bank_for_tests,
    traits::{TransactionsProcessor, TransactionsProcessorProcessResult},
};

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
        let genesis_config = create_genesis_config_with_leader_and_fees(
            u64::MAX,
            &Pubkey::new_unique(),
        )
        .genesis_config;
        let bank = Arc::new(bank_for_tests(&genesis_config, None, None));
        Self::new(bank)
    }
}

impl TransactionsProcessor for BankTransactionsProcessor {
    fn process(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<TransactionsProcessorProcessResult, String> {
        let transactions: Vec<SanitizedTransaction> = transactions
            .into_iter()
            .map(SanitizedTransaction::from_transaction_for_tests)
            .collect();
        self.process_sanitized(transactions)
    }

    fn process_sanitized(
        &self,
        transactions: Vec<SanitizedTransaction>,
    ) -> Result<TransactionsProcessorProcessResult, String> {
        let mut transaction_outcomes = HashMap::new();

        for transaction in transactions {
            let signature = *transaction.signature();

            let txs = vec![transaction.clone()];
            let batch = self.bank.prepare_sanitized_batch(&txs);
            let mut timings = ExecuteTimings::default();
            let (
                TransactionResults {
                    execution_results, ..
                },
                _,
            ) = self.bank.load_execute_and_commit_transactions(
                &batch,
                true,
                TransactionExecutionRecordingOpts::recording_logs(),
                &mut timings,
                None,
            );

            let execution_result = execution_results
                .first()
                .expect("Could not find the transaction result");
            let execution_details = match execution_result.details() {
                Some(details) => details.clone(),
                None => panic!(
                    "Error resolving transaction results details: {:?}, tx: {:?}",
                    execution_result, transaction
                ),
            };

            transaction_outcomes
                .insert(signature, (transaction, execution_details));
        }

        Ok(TransactionsProcessorProcessResult {
            transactions: transaction_outcomes,
        })
    }

    fn bank(&self) -> &Bank {
        &self.bank
    }
}

#[cfg(test)]
mod tests {
    use magicblock_bank::bank_dev_utils::transactions::create_funded_accounts;
    use solana_sdk::{
        native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, system_transaction,
    };

    use super::*;
    use crate::{diagnostics::log_exec_details, init_logger};

    #[tokio::test]
    async fn test_system_transfer_enough_funds() {
        init_logger!();
        let tx_processor = BankTransactionsProcessor::default();
        let payers = create_funded_accounts(
            &tx_processor.bank,
            1,
            Some(LAMPORTS_PER_SOL),
        );
        let start_hash = tx_processor.bank.last_blockhash();
        let to = Pubkey::new_unique();
        let tx = system_transaction::transfer(
            &payers[0],
            &to,
            890_880_000,
            start_hash,
        );
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
        let payers =
            create_funded_accounts(&tx_processor.bank, 1, Some(890_850_000));
        let start_hash = tx_processor.bank.last_blockhash();
        let to = Pubkey::new_unique();
        let tx = system_transaction::transfer(
            &payers[0],
            &to,
            890_880_000,
            start_hash,
        );
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
