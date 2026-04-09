use magicblock_accounts_db::AccountsDbResult;
use magicblock_core::{
    link::{
        accounts::{AccountWithSlot, LockedAccount},
        transactions::{
            ProcessableTransaction, TransactionProcessingMode,
            TransactionSimulationResult, TransactionStatus,
            TxnSimulationResultTx,
        },
    },
    tls::ExecutionTlsStash,
};
use magicblock_metrics::metrics::{
    FAILED_TRANSACTIONS_COUNT, TRANSACTION_COUNT,
};
use solana_account::AccountSharedData;
use solana_compute_budget::compute_budget_limits::ComputeBudgetLimits;
use solana_fee_structure::FeeDetails;
use solana_program_runtime::execution_budget::SVMTransactionExecutionAndFeeBudgetLimits;
use solana_pubkey::Pubkey;
use solana_svm::{
    account_loader::CheckedTransactionDetails,
    rollback_accounts::RollbackAccounts,
    transaction_balances::BalanceCollector,
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

use crate::executor::IndexedTransaction;

impl super::TransactionExecutor {
    /// Executes a transaction and conditionally commits its results.
    ///
    /// # Arguments
    /// * `transaction` - The transaction to execute
    /// * `tx` - Channel to send the execution result (None for replay)
    /// * `persist` - Controls persistence behavior:
    ///   - `None`: Execution mode - notify subscribers, record to ledger, process tasks
    ///   - `Some(true)`: Replay with persist - record to ledger, no notifications
    ///   - `Some(false)`: Replay without persist - no side effects
    pub(super) fn execute(
        &self,
        mut transaction: IndexedTransaction,
        persist: Option<bool>,
    ) {
        TRANSACTION_COUNT.inc();
        let (result, balances) = {
            let txn = [transaction.txn.transaction];
            let result = self.process(&txn);
            let [txn] = txn;
            transaction.txn.transaction = txn;
            result
        };

        // 1. Handle Loading/Processing Failures
        let processed = match result {
            Ok(processed) => processed,
            Err(err) => {
                return self.handle_failure(transaction, err, None);
            }
        };

        // 2. Commit Account State (DB Update)
        // Note: Failed transactions still pay fees, so we attempt commit even on execution failure.
        let fee_payer = *transaction.fee_payer();
        // Only send account updates for Execution mode (persist is None)
        let notify = persist.is_none();
        if let Err(err) = self.commit_accounts(fee_payer, &processed, notify) {
            return self.handle_failure(
                transaction,
                TransactionError::CommitCancelled,
                Some(vec![err.to_string()]),
            );
        }

        let status = processed.status();

        // 3. Post-Processing (Tasks & Ledger)
        // Only process scheduled tasks for successful transactions in Execution mode
        if status.is_ok() && persist.is_none() {
            self.process_scheduled_tasks();
        }
        let tx = if let TransactionProcessingMode::Execution(ref mut tx) =
            transaction.txn.mode
        {
            tx.take()
        } else {
            None
        };
        // Record to ledger for Execution mode (persist is None) or Replay with persist=true
        if persist.unwrap_or(true) {
            if let Err(err) =
                self.record_transaction(transaction, processed, balances)
            {
                error!(error = ?err, "Failed to record transaction to ledger");
            }
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
        let number_of_accounts = transaction[0].message().account_keys().len();
        let (result, _) = self.process(&transaction);
        let simulation_result = match result {
            Ok(processed) => {
                let status = processed.status();
                let units_consumed = processed.executed_units();
                let (
                    logs,
                    post_simulation_accounts,
                    return_data,
                    inner_instructions,
                ) = match processed {
                    ProcessedTransaction::Executed(executed) => {
                        let execution_details = executed.execution_details;
                        let post_simulation_accounts = executed
                            .loaded_transaction
                            .accounts
                            .into_iter()
                            .take(number_of_accounts)
                            .collect();
                        (
                            execution_details.log_messages,
                            post_simulation_accounts,
                            execution_details.return_data,
                            execution_details.inner_instructions,
                        )
                    }
                    ProcessedTransaction::FeesOnly(_) => {
                        (None, vec![], None, None)
                    }
                };
                TransactionSimulationResult {
                    result: status,
                    units_consumed,
                    logs,
                    post_simulation_accounts,
                    return_data,
                    inner_instructions,
                }
            }
            Err(error) => TransactionSimulationResult {
                result: Err(error),
                units_consumed: 0,
                logs: Default::default(),
                post_simulation_accounts: vec![],
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
    ) -> (TransactionProcessingResult, Option<BalanceCollector>) {
        let limits = self.compute_budget_limits(&txn[0]);
        let checked = CheckedTransactionDetails::new(None, limits);
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

        (result, output.balance_collector)
    }

    /// Common handler for transaction failures (load error or commit error).
    fn handle_failure(
        &self,
        mut txn: IndexedTransaction,
        err: TransactionError,
        logs: Option<Vec<String>>,
    ) {
        FAILED_TRANSACTIONS_COUNT.inc();

        // Even on failure, ensure stash is clear (though likely empty if load failed).
        ExecutionTlsStash::clear();

        if let TransactionProcessingMode::Execution(ref mut tx) = txn.txn.mode {
            if let Some(tx) = tx.take() {
                let _ = tx.send(Err(err.clone()));
            }
        }
        self.record_failure(txn, Err(err), logs);
    }

    fn process_scheduled_tasks(&self) {
        while let Some(task) = ExecutionTlsStash::next_task() {
            if let Err(e) = self.tasks_tx.send(task) {
                error!(error = ?e, "Scheduled tasks service disconnected");
            }
        }
    }

    /// Writes a fully processed transaction to the Ledger.
    fn record_transaction(
        &self,
        txn: IndexedTransaction,
        result: ProcessedTransaction,
        balances: Option<BalanceCollector>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (pre_balances, post_balances) =
            transaction_balances(&txn, balances);
        let meta = match result {
            ProcessedTransaction::Executed(executed) => TransactionStatusMeta {
                fee: executed.loaded_transaction.fee_details.total_fee(),
                compute_units_consumed: Some(
                    executed.execution_details.executed_units,
                ),
                status: executed.execution_details.status,
                pre_balances,
                post_balances,
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
                pre_balances,
                post_balances,
                loaded_addresses: txn.get_loaded_addresses(),
                ..Default::default()
            },
        };

        self.write_to_ledger(txn, meta)
    }

    /// Writes a failed transaction (load or commit error) to the Ledger.
    fn record_failure(
        &self,
        txn: IndexedTransaction,
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
        if let Err(err) = self.write_to_ledger(txn, meta) {
            error!(error = ?err, "Failed to record failed transaction to ledger");
        }
    }

    fn write_to_ledger(
        &self,
        txn: IndexedTransaction,
        meta: TransactionStatusMeta,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let signature = *txn.signature();
        let slot = txn.slot;
        let index = txn.index;

        let ProcessableTransaction {
            transaction,
            encoded,
            ..
        } = txn.txn;

        // Use pre-encoded bytes or serialize on the spot
        let encoded = match encoded {
            Some(bytes) => bytes,
            None => {
                let versioned = transaction.to_versioned_transaction();
                bincode::serialize(&versioned)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?
            }
        };

        let tx_account_locks = transaction.get_account_locks_unchecked();

        let result = self.ledger.write_transaction(
            signature,
            slot,
            index,
            tx_account_locks.writable,
            tx_account_locks.readonly,
            &encoded,
            meta.clone(),
        );
        if let Err(error) = result {
            error!(error = ?error, "Failed to commit transaction to ledger");
            return Err(error.into());
        }

        let status = TransactionStatus {
            slot,
            index,
            txn: transaction,
            meta,
        };

        // Notify listeners
        let _ = self.transaction_tx.send(status);
        Ok(())
    }

    /// Persists account changes to AccountsDb and notifies listeners.
    fn commit_accounts(
        &self,
        fee_payer: Pubkey,
        result: &ProcessedTransaction,
        notify: bool,
    ) -> AccountsDbResult<()> {
        let succeeded = result.status().is_ok();
        let accounts = match result {
            ProcessedTransaction::Executed(executed) => {
                if succeeded && !executed.programs_modified_by_tx.is_empty() {
                    self.processor.global_program_cache.write().unwrap().merge(
                        &self.processor.environments,
                        &executed.programs_modified_by_tx,
                    );
                }

                if !succeeded {
                    &executed.loaded_transaction.accounts[..1]
                } else {
                    &executed.loaded_transaction.accounts
                }
            }
            ProcessedTransaction::FeesOnly(fo) => {
                if let RollbackAccounts::FeePayerOnly { fee_payer: account } =
                    &fo.rollback_accounts
                {
                    return self.insert_and_notify(
                        &[(fee_payer, account.1.clone())],
                        notify,
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

        self.insert_and_notify(accounts, notify, privileged)
    }

    fn insert_and_notify(
        &self,
        accounts: &[(Pubkey, AccountSharedData)],
        notify: bool,
        privileged: bool,
    ) -> AccountsDbResult<()> {
        // Filter: Persist only dirty or privileged accounts
        let to_commit = accounts
            .iter()
            .filter(|(_, acc)| privileged || acc.is_dirty());

        self.accountsdb.insert_batch(to_commit)?;

        if !notify {
            return Ok(());
        }

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

        // 2. Confined Account Integrity Check
        // Confined accounts must not have their lamport balance changed.
        for (pubkey, acc) in &txn.accounts {
            if !acc.confined() {
                continue;
            }
            if acc.lamports_changed() {
                executed.execution_details.status =
                    Err(TransactionError::UnbalancedTransaction);
                logs.push(format!(
                    "Confined account {pubkey} has been illegally modified"
                ));
                break;
            }
        }
    }

    fn compute_budget_limits(
        &self,
        txn: &SanitizedTransaction,
    ) -> SVMTransactionExecutionAndFeeBudgetLimits {
        let limits = ComputeBudgetLimits::default();
        let signature_fee = signature_fee(
            txn,
            self.environment.blockhash_lamports_per_signature,
        );
        let fee_details = FeeDetails::new(signature_fee, 0);

        limits.get_compute_budget_and_limits(
            limits.loaded_accounts_bytes,
            fee_details,
            false,
        )
    }
}

fn transaction_balances(
    txn: &IndexedTransaction,
    balances: Option<BalanceCollector>,
) -> (Vec<u64>, Vec<u64>) {
    let count = txn.message().account_keys().len();
    let Some(balances) = balances else {
        return (vec![0; count], vec![0; count]);
    };

    let (mut pre, mut post, _, _) = balances.into_vecs();
    (
        pre.pop().unwrap_or_else(|| vec![0; count]),
        post.pop().unwrap_or_else(|| vec![0; count]),
    )
}

fn signature_fee(
    txn: &SanitizedTransaction,
    lamports_per_signature: u64,
) -> u64 {
    txn.message()
        .num_transaction_signatures()
        .saturating_mul(lamports_per_signature)
}
