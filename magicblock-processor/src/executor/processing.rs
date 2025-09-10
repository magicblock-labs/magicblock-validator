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
    /// Executes a transaction and conditionally commits its results to the
    /// `AccountsDb` and `Ledger`.
    ///
    /// This is the primary entry point for processing transactions
    /// that are intended to change the state of the blockchain.
    ///
    /// ## Commitment Logic
    /// - **Successful transactions** are fully committed: account changes are saved to
    ///   the `AccountsDb`, and the transaction itself is written to the `Ledger`.
    /// - **"Fire-and-forget" failed transactions** (`tx` is `None`) have only the fee
    ///   deducted from the payer account, which is then saved to the `AccountsDb`.
    /// - **Awaited failed transactions** (`tx` is `Some`, e.g., an RPC preflight check)
    ///   are **not committed** at all; their results are returned directly to the caller
    ///   without any state changes.
    /// - **Replayed transactions** (`is_replay` is `true`) commit account changes but do
    ///   not write the transaction to the ledger, as it's already there.
    pub(super) fn execute(
        &self,
        transaction: [SanitizedTransaction; 1],
        tx: TxnExecutionResultTx,
        is_replay: bool,
    ) {
        let (result, balances) = self.process(&transaction);
        let [txn] = transaction;

        // If the transaction fails to load entirely, we don't commit anything.
        let result = result.and_then(|mut processed| {
            let result = processed.status();

            // If the transaction failed and the caller is waiting
            // for the result, do not persist any changes.
            if result.is_err() && tx.is_some() {
                return result;
            }

            // Otherwise, check that the transaction didn't violate any permissions
            // Self::validate_account_access(txn.message(), &processed)?;
            // And commit the account state changes if all is good
            self.commit_accounts(&mut processed, is_replay);

            // For new transactions, also commit the transaction to the ledger.
            if !is_replay {
                self.commit_transaction(txn, processed, balances);
            }
            result
        });

        // Send the final result back to the caller if they are waiting.
        tx.map(|tx| tx.send(result));
    }

    /// Executes a transaction in a simulated, ephemeral environment.
    ///
    /// This method runs a transaction through the SVM but **never persists any state changes**
    /// to the `AccountsDb` or `Ledger`. It returns a more detailed set of execution
    /// results, including compute units, logs, and return data, which is required by
    /// RPC `simulateTransaction` call.
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

    /// A convenience helper that wraps the core Solana SVM `load_and_execute` function.
    /// It serves as the bridge between the executor's logic and the underlying SVM engine.
    fn process(
        &self,
        txn: &[SanitizedTransaction; 1],
    ) -> (TransactionProcessingResult, AccountsBalances) {
        let checked = CheckedTransactionDetails::new(
            None,
            self.environment.fee_lamports_per_signature,
        );
        let mut output =
            self.processor.load_and_execute_sanitized_transactions(
                self,
                txn,
                vec![Ok(checked); 1],
                &self.environment,
                &self.config,
            );
        // SAFETY:
        // we passed a single transaction for execution, and
        // we will get a guaranteed single result back.
        let result = output.processing_results.pop().expect(
            "single transaction result is always present in the output",
        );
        (result, output.balances)
    }

    /// A helper method that persists a transaction and its metadata to
    /// the ledger. After a successful write, it also forwards the
    /// `TransactionStatus` to the rest of the system via corresponding channel.
    fn commit_transaction(
        &self,
        txn: SanitizedTransaction,
        result: ProcessedTransaction,
        balances: AccountsBalances,
    ) {
        let meta = match result {
            ProcessedTransaction::Executed(executed) => TransactionStatusMeta {
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
            },
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
                accounts: txn
                    .message()
                    .account_keys()
                    .iter()
                    .copied()
                    .collect(),
                logs: meta.log_messages.clone(),
            },
        };
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
        // Send the final status to the listeners (EventProcessor workers).
        let _ = self.transaction_tx.send(status);
    }

    /// A helper method that persists modified account states to the `AccountsDb`.
    fn commit_accounts(
        &self,
        result: &mut ProcessedTransaction,
        is_replay: bool,
    ) {
        let ProcessedTransaction::Executed(executed) = result else {
            return;
        };
        let succeeded = executed.was_successful();
        if !succeeded {
            // For failed transactions, only persist the payer's account to charge the fee.
            executed.loaded_transaction.accounts.drain(1..);
        }
        let programs = &executed.programs_modified_by_tx;
        if !programs.is_empty() && succeeded {
            self.processor
                .program_cache
                .write()
                .unwrap()
                .merge(programs);
        }
        for (pubkey, account) in executed.loaded_transaction.accounts.drain(..)
        {
            // only persist account's update if it was actually modified, ignore
            // the rest, even if an account was writeable in the transaction
            if !account.is_dirty() {
                continue;
            }
            self.accountsdb.insert_account(&pubkey, &account);

            if is_replay {
                continue;
            }
            let account = AccountWithSlot {
                slot: self.processor.slot,
                account: LockedAccount::new(pubkey, account),
            };
            let _ = self.accounts_tx.send(account);
        }
    }
}
