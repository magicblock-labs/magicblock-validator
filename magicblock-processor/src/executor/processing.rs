use magicblock_accounts_db::AccountsDbResult;
use magicblock_core::{
    link::{
        accounts::{AccountWithSlot, LockedAccount},
        transactions::{
            TransactionSimulationResult, TransactionStatus,
            TxnExecutionResultTx, TxnSimulationResultTx,
        },
    },
    tls::ExecutionTlsStash,
};
use magicblock_metrics::metrics::{
    FAILED_TRANSACTIONS_COUNT, TRANSACTION_COUNT,
};
use solana_pubkey::Pubkey;
use solana_svm::{
    account_loader::{AccountsBalances, CheckedTransactionDetails},
    rollback_accounts::RollbackAccounts,
    transaction_processing_result::{
        ProcessedTransaction, TransactionProcessingResult,
    },
};
use solana_svm_transaction::svm_message::SVMMessage;
use solana_transaction::sanitized::SanitizedTransaction;
use solana_transaction_error::{TransactionError, TransactionResult};
use solana_transaction_status::{
    map_inner_instructions, TransactionStatusMeta,
};
use tracing::*;

impl super::TransactionExecutor {
    /// Executes a transaction and conditionally commits its results.
    pub(super) fn execute(
        &self,
        transaction: [SanitizedTransaction; 1],
        tx: TxnExecutionResultTx,
        is_replay: bool,
    ) {
        TRANSACTION_COUNT.inc();
        let (result, balances) = self.process(&transaction);
        let [txn] = transaction;

        // 1. Handle Loading/Processing Failures
        let processed = match result {
            Ok(processed) => processed,
            Err(err) => {
                return self.handle_failure(txn, err, None, tx);
            }
        };

        // 2. Commit Account State (DB Update)
        // Note: Failed transactions still pay fees, so we attempt commit even on execution failure.
        let fee_payer = *txn.fee_payer();
        if let Err(err) = self.commit_accounts(fee_payer, &processed, is_replay)
        {
            return self.handle_failure(
                txn,
                TransactionError::CommitCancelled,
                Some(vec![err.to_string()]),
                tx,
            );
        }

        let status = processed.status();

        // 3. Post-Processing (Tasks & Ledger)
        if status.is_ok() && !is_replay {
            self.process_scheduled_tasks();
        }

        if !is_replay {
            self.record_transaction(txn, processed, balances);
        }

        ExecutionTlsStash::clear();
        if let Some(tx) = tx {
            let _ = tx.send(status);
        }
    }

    /// Executes a transaction in simulation mode (no state persistence).
    pub(super) fn simulate(
        &self,
        transaction: [SanitizedTransaction; 1],
        tx: TxnSimulationResultTx,
    ) {
        let (result, _) = self.process(&transaction);
        let simulation_result = match result {
            Ok(processed) => {
                let status = processed.status();
                let units_consumed = processed.executed_units();
                let (logs, return_data, inner_instructions) = match processed {
                    ProcessedTransaction::Executed(ex) => (
                        ex.execution_details.log_messages,
                        ex.execution_details.return_data,
                        ex.execution_details.inner_instructions,
                    ),
                    ProcessedTransaction::FeesOnly(_) => Default::default(),
                };
                TransactionSimulationResult {
                    result: status,
                    units_consumed,
                    logs,
                    return_data,
                    inner_instructions,
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

        ExecutionTlsStash::clear();
        let _ = tx.send(simulation_result);
    }

    /// Wraps the SVM load_and_execute logic.
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

        let mut result = output
            .processing_results
            .pop()
            .expect("single transaction result is guaranteed");

        if let Ok(ref mut processed) = result {
            self.verify_account_states(processed);
        }

        (result, output.balances)
    }

    /// Common handler for transaction failures (load error or commit error).
    fn handle_failure(
        &self,
        txn: SanitizedTransaction,
        err: TransactionError,
        logs: Option<Vec<String>>,
        tx: TxnExecutionResultTx,
    ) {
        FAILED_TRANSACTIONS_COUNT.inc();
        self.record_failure(txn, Err(err.clone()), logs);

        // Even on failure, ensure stash is clear (though likely empty if load failed).
        ExecutionTlsStash::clear();

        if let Some(tx) = tx {
            let _ = tx.send(Err(err));
        }
    }

    fn process_scheduled_tasks(&self) {
        while let Some(task) = ExecutionTlsStash::next_task() {
            if let Err(e) = self.tasks_tx.send(task) {
                error!("Scheduled tasks service disconnected: {e}");
            }
        }
    }

    /// Writes a fully processed transaction to the Ledger.
    fn record_transaction(
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

        self.write_to_ledger(txn, meta);
    }

    /// Writes a failed transaction (load or commit error) to the Ledger.
    fn record_failure(
        &self,
        txn: SanitizedTransaction,
        status: TransactionResult<()>,
        logs: Option<Vec<String>>,
    ) {
        let count = txn.message().account_keys().len();
        let meta = TransactionStatusMeta {
            status,
            pre_balances: vec![0; count],
            post_balances: vec![0; count],
            log_messages: logs,
            ..Default::default()
        };
        self.write_to_ledger(txn, meta);
    }

    fn write_to_ledger(
        &self,
        txn: SanitizedTransaction,
        meta: TransactionStatusMeta,
    ) {
        let signature = *txn.signature();
        let index = match self.ledger.write_transaction(
            signature,
            self.processor.slot,
            &txn,
            // TODO(bmuddha): perf: remove clone with the new ledger
            meta.clone(),
        ) {
            Ok(i) => i,
            Err(error) => {
                error!("failed to commit transaction to the ledger: {error}");
                return;
            }
        };

        let status = TransactionStatus {
            slot: self.processor.slot,
            index,
            txn,
            meta,
        };

        // Notify listeners
        let _ = self.transaction_tx.send(status);
    }

    /// Persists account changes to AccountsDb and notifies listeners.
    fn commit_accounts(
        &self,
        fee_payer: Pubkey,
        result: &ProcessedTransaction,
        is_replay: bool,
    ) -> AccountsDbResult<()> {
        let succeeded = result.status().is_ok();
        let accounts = match result {
            ProcessedTransaction::Executed(executed) => {
                if succeeded && !executed.programs_modified_by_tx.is_empty() {
                    self.processor
                        .program_cache
                        .write()
                        .unwrap()
                        .merge(&executed.programs_modified_by_tx);
                }

                if !succeeded {
                    // Only charge fee payer on failure
                    &executed.loaded_transaction.accounts[..1]
                } else {
                    &executed.loaded_transaction.accounts
                }
            }
            ProcessedTransaction::FeesOnly(fo) => {
                if let RollbackAccounts::FeePayerOnly { fee_payer_account } =
                    &fo.rollback_accounts
                {
                    // Temporary slice construction to match expected type
                    // This is slightly inefficient but safe; the vector here is tiny (1 item)
                    return self.insert_and_notify(
                        &[(fee_payer, fee_payer_account.clone())],
                        is_replay,
                        false,
                    );
                }
                return Ok(());
            }
        };

        let privileged = accounts
            .first()
            .map(|(_, acc)| acc.privileged())
            .unwrap_or(false);

        self.insert_and_notify(accounts, is_replay, privileged)
    }

    fn insert_and_notify(
        &self,
        accounts: &[(Pubkey, solana_account::AccountSharedData)],
        is_replay: bool,
        privileged: bool,
    ) -> AccountsDbResult<()> {
        // Filter: Persist only dirty or privileged accounts
        let to_commit = accounts
            .iter()
            .filter(|(_, acc)| acc.is_dirty() || privileged);

        self.accountsdb.insert_batch(to_commit)?;

        if is_replay {
            return Ok(());
        }

        // Notify downstream
        for (pubkey, account) in accounts {
            let update = AccountWithSlot {
                slot: self.processor.slot,
                account: LockedAccount::new(*pubkey, account.clone()),
            };
            let _ = self.accounts_tx.send(update);
        }
        Ok(())
    }

    fn verify_account_states(&self, processed: &mut ProcessedTransaction) {
        let ProcessedTransaction::Executed(executed) = processed else {
            return;
        };
        let txn = &executed.loaded_transaction;
        let Some((_, fee_payer_acc)) = txn.accounts.first() else {
            return;
        };

        // Privileged fee payers bypass all checks
        if fee_payer_acc.privileged() {
            return;
        }

        let logs = executed
            .execution_details
            .log_messages
            .get_or_insert_default();

        // 1. Gasless Mode Integrity Check
        // In gasless mode, non-delegated fee payers must NOT be modified (drained).
        if self.environment.fee_lamports_per_signature == 0 {
            let lamports_changed = fee_payer_acc
                .as_borrowed()
                .map(|a| a.lamports_changed())
                .unwrap_or(true); // Owned/resized implies changed

            if !self.is_auto_airdrop_lamports_enabled
                && lamports_changed
                && !fee_payer_acc.delegated()
            {
                executed.execution_details.status =
                    Err(TransactionError::InvalidAccountForFee);
                logs.push(
                    "Feepayer balance has been illegally modified".into(),
                );
                return;
            }
        }

        // 2. Confined Account Integrity Check
        // Confined accounts must not have their lamport balance changed.
        for (pubkey, acc) in &txn.accounts {
            if acc.confined() {
                let lamports_changed = acc
                    .as_borrowed()
                    .map(|a| a.lamports_changed())
                    .unwrap_or(true);

                if lamports_changed {
                    executed.execution_details.status =
                        Err(TransactionError::UnbalancedTransaction);
                    logs.push(format!(
                        "Confined account {pubkey} has been illegally modified"
                    ));
                    break;
                }
            }
        }
    }
}
