use std::sync::Arc;

use sleipnir_bank::bank::Bank;
use sleipnir_processor::batch_processor::{
    execute_batch, TransactionBatchWithIndexes,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::{
    signature::Signature,
    transaction::{SanitizedTransaction, Transaction},
};

use crate::errors::AccountsResult;

pub fn accounts_execute_transaction(
    tx: Transaction,
    bank: &Arc<Bank>,
    transaction_status_sender: Option<&TransactionStatusSender>,
) -> AccountsResult<Signature> {
    let sanitized_tx = SanitizedTransaction::try_from_legacy_transaction(tx)?;
    let signature = *sanitized_tx.signature();
    let txs = &[sanitized_tx];
    let batch = bank.prepare_sanitized_batch(txs);

    let batch_with_indexes = TransactionBatchWithIndexes {
        batch,
        transaction_indexes: txs
            .iter()
            .enumerate()
            .map(|(idx, _)| idx)
            .collect(),
    };
    let mut timings = Default::default();
    execute_batch(
        &batch_with_indexes,
        bank,
        transaction_status_sender,
        &mut timings,
        None,
    )?;
    Ok(signature)
}
