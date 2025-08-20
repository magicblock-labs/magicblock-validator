use std::sync::atomic::Ordering;

use log::error;
use solana_svm::{
    account_loader::{AccountsBalances, CheckedTransactionDetails},
    transaction_processing_result::{
        ProcessedTransaction, TransactionProcessingResult,
    },
};
use solana_transaction::sanitized::SanitizedTransaction;
use solana_transaction_status::{
    map_inner_instructions, TransactionStatusMeta,
};

use magicblock_core::link::{
    accounts::{AccountWithSlot, LockedAccount},
    transactions::{
        TransactionExecutionResult, TransactionSimulationResult,
        TransactionStatus, TxnExecutionResultTx, TxnSimulationResultTx,
    },
};

impl super::TransactionExecutor {
    pub(super) fn execute(
        &self,
        transaction: [SanitizedTransaction; 1],
        tx: TxnExecutionResultTx,
        is_replay: bool,
    ) {
        let (result, balances) = self.process(&transaction);
        let [txn] = transaction;
        let result = result.and_then(|processed| {
            let result = processed.status();
            self.commit(txn, processed, balances, is_replay);
            result
        });
        tx.map(|tx| tx.send(result));
    }

    pub(super) fn simulate(
        &self,
        transaction: [SanitizedTransaction; 1],
        tx: TxnSimulationResultTx,
    ) {
        let (result, _) = self.process(&transaction);
        let result = match result {
            Ok(processed) => {
                let result = processed.status();
                let units_consumed = processed.executed_units();
                let (logs, data, ixs) = match processed {
                    ProcessedTransaction::Executed(ex) => (
                        ex.execution_details.log_messages,
                        ex.execution_details.return_data,
                        ex.execution_details.inner_instructions,
                    ),
                    ProcessedTransaction::FeesOnly(_) => Default::default(),
                };
                TransactionSimulationResult {
                    result,
                    units_consumed,
                    logs,
                    return_data: data,
                    inner_instructions: ixs,
                }
            }
            Err(error) => TransactionSimulationResult {
                result: Err(error),
                units_consumed: 0,
                logs: Default::default(),
                return_data: None,
                inner_instructions: None,
            },
        };
        let _ = tx.send(result);
    }

    fn process(
        &self,
        txn: &[SanitizedTransaction],
    ) -> (TransactionProcessingResult, AccountsBalances) {
        let checked = CheckedTransactionDetails::new(
            None,
            self.environment.fee_lamports_per_signature,
        );
        let mut output =
            self.processor.load_and_execute_sanitized_transactions(
                self,
                &txn,
                vec![Ok(checked); 1],
                &self.environment,
                &self.config,
            );
        let result = output.processing_results.pop().expect(
            "single transaction result is always present in the output",
        );
        (result, output.balances)
    }

    fn commit(
        &self,
        txn: SanitizedTransaction,
        result: ProcessedTransaction,
        balances: AccountsBalances,
        is_replay: bool,
    ) {
        let mut accounts = Vec::new();

        let meta = match result {
            ProcessedTransaction::Executed(executed) => {
                let programs = &executed.programs_modified_by_tx;
                if !programs.is_empty() && executed.was_successful() {
                    self.processor
                        .program_cache
                        .write()
                        .unwrap()
                        .merge(programs);
                }
                accounts = executed.loaded_transaction.accounts;
                TransactionStatusMeta {
                    fee: executed.loaded_transaction.fee_details.total_fee(),
                    compute_units_consumed: Some(
                        executed.execution_details.executed_units,
                    ),
                    status: executed.execution_details.status,
                    pre_balances: balances.pre,
                    post_balances: balances.post,
                    log_messages: executed.execution_details.log_messages,
                    loaded_addresses: txn.get_loaded_addresses(),
                    return_data: executed.execution_details.return_data,
                    inner_instructions: executed
                        .execution_details
                        .inner_instructions
                        .map(map_inner_instructions)
                        .map(|i| i.collect()),
                    ..Default::default()
                }
            }
            ProcessedTransaction::FeesOnly(fo) => TransactionStatusMeta {
                fee: fo.fee_details.total_fee(),
                status: Err(fo.load_error),
                pre_balances: balances.pre,
                post_balances: balances.post,
                loaded_addresses: txn.get_loaded_addresses(),
                ..Default::default()
            },
        };
        let signature = *txn.signature();
        let status = TransactionStatus {
            signature,
            slot: self.processor.slot,
            result: TransactionExecutionResult {
                result: meta.status.clone(),
                // TODO(bmuddha) perf: avoid allocation with the new ledger impl
                accounts: txn
                    .message()
                    .account_keys()
                    .iter()
                    .copied()
                    .collect(),
                // TODO(bmuddha) perf: avoid cloning with the new ledger impl
                logs: meta.log_messages.clone(),
            },
        };
        for (pubkey, account) in accounts {
            if !account.is_dirty() {
                continue;
            }
            self.accountsdb.insert_account(&pubkey, &account);
            let account = AccountWithSlot {
                slot: self.processor.slot,
                account: LockedAccount::new(pubkey, account),
            };
            let _ = self.accounts_tx.send(account);
        }
        if is_replay {
            return;
        }
        if let Err(error) = self.ledger.write_transaction(
            signature,
            self.processor.slot,
            txn,
            meta,
            self.index.fetch_add(1, Ordering::Relaxed),
        ) {
            error!("failed to commit transaction to the ledger: {error}");
            return;
        }
        let _ = self.transaction_tx.send(status);
    }
}
