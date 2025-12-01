use log::*;
use magicblock_core::{
    link::{
        accounts::{AccountWithSlot, LockedAccount},
        transactions::{
            TransactionExecutionResult, TransactionSimulationResult,
            TransactionStatus, TxnExecutionResultTx, TxnSimulationResultTx,
        },
    },
    tls::ExecutionTlsStash,
};
use magicblock_metrics::metrics::{
    FAILED_TRANSACTIONS_COUNT, TRANSACTION_COUNT,
};
use solana_account::ReadableAccount;
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
        TRANSACTION_COUNT.inc();

        let processed = match result {
            Ok(processed) => processed,
            Err(err) => {
                // Transaction failed to load, we persist it to the
                // ledger, only for the convenience of the user
                let status = Err(err);
                self.commit_failed_transaction(txn, status.clone());
                FAILED_TRANSACTIONS_COUNT.inc();
                tx.map(|tx| tx.send(status));
                // NOTE:
                // Transactions that failed to load, cannot have touched the thread
                // local storage, thus there's no need to clear it before returning
                return;
            }
        };

        // The transaction has been processed, we can commit the account state changes
        // NOTE:
        // Failed transactions still pay fees, so we need to
        // commit the accounts even if the transaction failed
        let feepayer = *txn.fee_payer();
        self.commit_accounts(feepayer, &processed, is_replay);

        let result = processed.status();
        if result.is_ok() && !is_replay {
            // If the transaction succeeded, check for potential tasks
            // that may have been scheduled during the transaction execution
            // TODO: send intents here as well once implemented
            while let Some(task) = ExecutionTlsStash::next_task() {
                // This is a best effort send, if the tasks service has terminated
                // for some reason, logging is the best we can do at this point
                let _ = self.tasks_tx.send(task).inspect_err(|_|
                    error!("Scheduled tasks service has hung up and is no longer running")
                );
            }
        }

        // We always commit transaction to the ledger (mostly for user convenience)
        if !is_replay {
            self.commit_transaction(txn, processed, balances);
        }

        // Make sure that no matter what happened to the transaction we clear the stash
        ExecutionTlsStash::clear();

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
        // Make sure that we clear the stash, so that simulations
        // don't interfere with actual transaction executions
        ExecutionTlsStash::clear();
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
        let mut result = output.processing_results.pop().expect(
            "single transaction result is always present in the output",
        );
        // Verify that account state invariants haven't been violated
        if let Ok(ref mut processed) = result {
            self.verify_account_states(processed);
        }

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
        ) {
            error!("failed to commit transaction to the ledger: {error}");
            return;
        }
        // Send the final status to the listeners (EventProcessor workers).
        let _ = self.transaction_tx.send(status);
    }

    /// A helper method that persists a transaction that couldn't even be loaded properly,
    /// to the ledger. This is done primarily for the convenience of the user, so that the
    /// status of transaction can always be queried, even if it didn't pass the load stage
    fn commit_failed_transaction(
        &self,
        txn: SanitizedTransaction,
        status: TransactionResult<()>,
    ) {
        let meta = TransactionStatusMeta {
            status,
            pre_balances: vec![0; txn.message().account_keys().len()],
            post_balances: vec![0; txn.message().account_keys().len()],
            ..Default::default()
        };
        let signature = *txn.signature();
        if let Err(error) = self.ledger.write_transaction(
            signature,
            self.processor.slot,
            txn,
            meta,
        ) {
            error!("failed to commit transaction to the ledger: {error}");
        }
    }

    /// A helper method that persists modified account states to the `AccountsDb`.
    fn commit_accounts(
        &self,
        feepayer: Pubkey,
        result: &ProcessedTransaction,
        is_replay: bool,
    ) {
        let succeeded = result.status().is_ok();
        let accounts = match result {
            ProcessedTransaction::Executed(executed) => {
                let programs = &executed.programs_modified_by_tx;
                if !programs.is_empty() && succeeded {
                    self.processor
                        .program_cache
                        .write()
                        .unwrap()
                        .merge(programs);
                }
                if !succeeded {
                    // For failed transactions, only persist the payer's account to charge the fee.
                    &executed.loaded_transaction.accounts[..1]
                } else {
                    &executed.loaded_transaction.accounts
                }
            }
            ProcessedTransaction::FeesOnly(fo) => {
                let RollbackAccounts::FeePayerOnly { fee_payer_account } =
                    &fo.rollback_accounts
                else {
                    return;
                };
                &[(feepayer, fee_payer_account.clone())]
            }
        };

        // The first loaded account is always a feepayer, check
        // whether we are running in privileged execution mode
        let privileged = accounts
            .first()
            .map(|feepayer| feepayer.1.privileged())
            .unwrap_or_default();

        for (pubkey, account) in accounts {
            // only persist account's update if it was actually modified, ignore
            // the rest, even if an account was writeable in the transaction.
            //
            // We also don't persist accounts that are empty, with an exception
            // for special cases, when those are inserted forcefully as placeholders
            // (for example by the chainlink), those cases can be distinguished from
            // others by the fact that such a transaction is always running in a
            // privileged mode.
            if !account.is_dirty() || (account.lamports() == 0 && !privileged) {
                continue;
            }
            self.accountsdb.insert_account(pubkey, account);

            if is_replay {
                continue;
            }
            let account = AccountWithSlot {
                slot: self.processor.slot,
                account: LockedAccount::new(*pubkey, account.clone()),
            };
            let _ = self.accounts_tx.send(account);
        }
    }

    /// Ensure that no post execution account state violations occurred:
    /// 1. No modification of the non-delegated feepayer in gasless mode
    /// 2. No illegal account resizing when the balance is zero
    fn verify_account_states(&self, processed: &mut ProcessedTransaction) {
        let ProcessedTransaction::Executed(executed) = processed else {
            return;
        };
        let txn = &executed.loaded_transaction;
        let feepayer = txn.accounts.first();
        let rollback_lamports =
            rollback_feepayer_lamports(&txn.rollback_accounts);

        let gasless = self.environment.fee_lamports_per_signature == 0;
        if gasless {
            // If we are running in the gasless mode, we should not allow
            // any mutation of the feepayer account, since that would make
            // it possible for malicious actors to peform transfer operations
            // from undelegated feepayers to delegated accounts, which would
            // result in validator loosing funds upon balance settling.
            let undelegated_feepayer_was_modified = feepayer
                .map(|acc| {
                    (acc.1.is_dirty()
                        && !self.is_auto_airdrop_lamports_enabled
                        && (acc.1.lamports() != 0 || rollback_lamports != 0))
                        && !acc.1.delegated()
                        && !acc.1.privileged()
                })
                .unwrap_or_default();
            if undelegated_feepayer_was_modified {
                executed.execution_details.status =
                    Err(TransactionError::InvalidAccountForFee);
                let logs = executed
                    .execution_details
                    .log_messages
                    .get_or_insert_default();
                let msg = "Feepayer balance has been modified illegally".into();
                logs.push(msg);
            }
        }
    }
}

// A utility to extract the rollback lamports of the feepayer
fn rollback_feepayer_lamports(rollback: &RollbackAccounts) -> u64 {
    match rollback {
        RollbackAccounts::FeePayerOnly { fee_payer_account } => {
            fee_payer_account.lamports()
        }
        RollbackAccounts::SameNonceAndFeePayer { nonce } => {
            nonce.account().lamports()
        }
        RollbackAccounts::SeparateNonceAndFeePayer {
            fee_payer_account,
            ..
        } => fee_payer_account.lamports(),
    }
}
