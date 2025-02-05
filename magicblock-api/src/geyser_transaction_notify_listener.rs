use std::sync::Arc;

use crossbeam_channel::Receiver;
use itertools::izip;
use magicblock_bank::{bank::Bank, geyser::TransactionNotifierImpl};
use magicblock_ledger::Ledger;
use magicblock_metrics::metrics;
use magicblock_transaction_status::{
    extract_and_fmt_memos, map_inner_instructions, TransactionStatusBatch,
    TransactionStatusMessage, TransactionStatusMeta,
};
use solana_rpc::transaction_notifier_interface::TransactionNotifier;
use solana_svm::transaction_commit_result::CommittedTransaction;

pub struct GeyserTransactionNotifyListener {
    transaction_notifier: Option<TransactionNotifierImpl>,
    transaction_recvr: Receiver<TransactionStatusMessage>,
    ledger: Arc<Ledger>,
}

impl GeyserTransactionNotifyListener {
    pub fn new(
        transaction_notifier: Option<TransactionNotifierImpl>,
        transaction_recvr: Receiver<TransactionStatusMessage>,
        ledger: Arc<Ledger>,
    ) -> Self {
        Self {
            transaction_notifier,
            transaction_recvr,
            ledger,
        }
    }

    pub fn run(
        &mut self,
        enable_rpc_transaction_history: bool,
        bank: Arc<Bank>,
    ) {
        let transaction_notifier = match self.transaction_notifier.take() {
            Some(notifier) => notifier,
            None => return,
        };
        let transaction_recvr = self.transaction_recvr.clone();
        let ledger = self.ledger.clone();
        // TODO(thlorenz): need to be able to cancel this
        std::thread::spawn(move || {
            while let Ok(message) = transaction_recvr.recv() {
                // Mostly from: rpc/src/transaction_status_service.rs
                match message {
                    TransactionStatusMessage::Batch(
                        TransactionStatusBatch {
                            slot,
                            transactions,
                            commit_results,
                            balances,
                            token_balances,
                            transaction_indexes,
                        },
                    ) => {
                        for (
                            transaction,
                            commit_result,
                            pre_balances,
                            post_balances,
                            pre_token_balances,
                            post_token_balances,
                            transaction_index,
                        ) in izip!(
                            transactions,
                            commit_results,
                            balances.pre_balances,
                            balances.post_balances,
                            token_balances.pre_token_balances,
                            token_balances.post_token_balances,
                            transaction_indexes,
                        ) {
                            if let Ok(details) = commit_result {
                                let CommittedTransaction {
                                    status,
                                    log_messages,
                                    inner_instructions,
                                    return_data,
                                    executed_units,
                                    ..
                                } = details;

                                let lamports_per_signature =
                                    bank.get_lamports_per_signature();
                                let fee = bank.get_fee_for_message_with_lamports_per_signature(
                                    transaction.message(),
                                    lamports_per_signature,
                                );

                                let fee_payer = transaction
                                    .message()
                                    .fee_payer()
                                    .to_string();
                                metrics::inc_transaction(
                                    status.is_ok(),
                                    &fee_payer,
                                );
                                metrics::inc_executed_units(executed_units);
                                metrics::inc_fee(fee);

                                let inner_instructions = inner_instructions
                                    .map(|inner_instructions| {
                                        map_inner_instructions(
                                            inner_instructions,
                                        )
                                        .collect()
                                    });
                                let pre_token_balances =
                                    Some(pre_token_balances);
                                let post_token_balances =
                                    Some(post_token_balances);
                                // NOTE: we don't charge rent and rewards are based on rent_debits
                                let rewards = None;
                                let loaded_addresses =
                                    transaction.get_loaded_addresses();
                                let transaction_status_meta =
                                    TransactionStatusMeta {
                                        status,
                                        fee,
                                        pre_balances,
                                        post_balances,
                                        inner_instructions,
                                        log_messages,
                                        pre_token_balances,
                                        post_token_balances,
                                        rewards,
                                        loaded_addresses,
                                        return_data,
                                        compute_units_consumed: Some(
                                            executed_units,
                                        ),
                                    };

                                transaction_notifier.notify_transaction(
                                    slot,
                                    transaction_index,
                                    transaction.signature(),
                                    &transaction_status_meta,
                                    &transaction,
                                );
                                if enable_rpc_transaction_history {
                                    if let Some(memos) = extract_and_fmt_memos(
                                        transaction.message(),
                                    ) {
                                        ledger
                                            .write_transaction_memos(transaction.signature(), slot, memos)
                                            .expect("Expect database write to succeed: TransactionMemos");
                                    }
                                    ledger.write_transaction(
                                        *transaction.signature(),
                                        slot,
                                        transaction,
                                        transaction_status_meta,
                                        transaction_index,
                                    )
                                        .expect("Expect database write to succeed: TransactionStatus");
                                }
                            }
                        }
                    }
                    TransactionStatusMessage::Freeze(_slot) => {}
                }
            }
        });
    }
}
