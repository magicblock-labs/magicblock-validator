use std::str::FromStr;

use log::{Level::Trace, *};
use magicblock_accounts_db::transaction_results::TransactionExecutionResult;
use magicblock_bank::bank::{Bank, TransactionExecutionRecordingOpts};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    clock::UnixTimestamp,
    hash::Hash,
    message::SanitizedMessage,
    transaction::{
        SanitizedTransaction, TransactionVerificationMode, VersionedTransaction,
    },
};
use solana_transaction_status::VersionedConfirmedBlock;

use crate::{
    errors::{LedgerError, LedgerResult},
    Ledger,
};

#[derive(Debug)]
struct PreparedBlock {
    slot: u64,
    previous_blockhash: Hash,
    blockhash: Hash,
    block_time: Option<UnixTimestamp>,
    transactions: Vec<VersionedTransaction>,
}

fn iter_blocks(
    ledger: &Ledger,
    mut prepared_block_handler: impl FnMut(PreparedBlock) -> LedgerResult<()>,
) -> LedgerResult<()> {
    let mut slot: u64 = 0;
    loop {
        let Ok(Some(block)) = ledger.get_block(slot) else {
            break;
        };
        let VersionedConfirmedBlock {
            blockhash,
            previous_blockhash,
            transactions,
            block_time,
            block_height,
            ..
        } = block;
        if let Some(block_height) = block_height {
            if slot != block_height {
                return Err(LedgerError::BlockStoreProcessor(format!(
                    "FATAL: block_height/slot mismatch: {} != {}",
                    slot, block_height
                )));
            }
        }

        // We only re-run transactions that succeeded since errored transactions
        // don't update any state
        let successfull_txs = transactions
            .into_iter()
            .filter(|tx| tx.meta.status.is_ok())
            .map(|tx| tx.transaction)
            .collect::<Vec<_>>();
        let previous_blockhash =
            Hash::from_str(&previous_blockhash).map_err(|err| {
                LedgerError::BlockStoreProcessor(format!(
                    "Failed to parse previous_blockhash: {:?}",
                    err
                ))
            })?;
        let blockhash = Hash::from_str(&blockhash).map_err(|err| {
            LedgerError::BlockStoreProcessor(format!(
                "Failed to parse blockhash: {:?}",
                err
            ))
        })?;

        prepared_block_handler(PreparedBlock {
            slot,
            previous_blockhash,
            blockhash,
            block_time,
            transactions: successfull_txs,
        })?;

        slot += 1;
    }
    Ok(())
}

pub fn process_ledger(ledger: &Ledger, bank: &Bank) -> LedgerResult<()> {
    iter_blocks(ledger, |prepared_block| {
        let mut block_txs = vec![];
        let Some(timestamp) = prepared_block.block_time else {
            return Err(LedgerError::BlockStoreProcessor(format!(
                "Block has no timestamp, {:?}",
                prepared_block
            )));
        };
        blockhash_log::log_blockhash(
            prepared_block.slot,
            &prepared_block.blockhash,
        );
        bank.replay_slot(
            prepared_block.slot,
            &prepared_block.previous_blockhash,
            &prepared_block.blockhash,
            timestamp as u64,
        );

        // Transactions are stored in the ledger ordered by most recent to latest
        // such to replay them in the order they executed we need to reverse them
        for tx in prepared_block.transactions.into_iter().rev() {
            match bank
                .verify_transaction(tx, TransactionVerificationMode::HashOnly)
            {
                Ok(tx) => block_txs.push(tx),
                Err(err) => {
                    return Err(LedgerError::BlockStoreProcessor(format!(
                        "Error processing transaction: {:?}",
                        err
                    )));
                }
            };
        }
        if !block_txs.is_empty() {
            // NOTE: ideally we would run all transactions in a single batch, but the
            // flawed account lock mechanism prevents this currently.
            // Until we revamp this transaction execution we execute each transaction
            // in its own batch.
            for tx in block_txs {
                log_sanitized_transaction(&tx);

                let mut timings = ExecuteTimings::default();
                let batch = [tx];
                let batch = bank.prepare_sanitized_batch(&batch);
                let (results, _) = bank.load_execute_and_commit_transactions(
                    &batch,
                    false,
                    TransactionExecutionRecordingOpts::recording_logs(),
                    &mut timings,
                    None,
                );

                log_execution_results(&results.execution_results);
                for result in results.execution_results {
                    if let TransactionExecutionResult::NotExecuted(err) =
                        &result
                    {
                        // If we're on trace log level then we already logged this above
                        if !log_enabled!(Trace) {
                            debug!(
                                "Transactions: {:#?}",
                                batch.sanitized_transactions()
                            );
                            debug!("Result: {:#?}", result);
                        }
                        return Err(LedgerError::BlockStoreProcessor(format!(
                            "Transaction {:?} could not be executed: {:?}",
                            result, err
                        )));
                    }
                }
            }
        }
        Ok(())
    })
}

fn log_sanitized_transaction(tx: &SanitizedTransaction) {
    if !log_enabled!(Trace) {
        return;
    }
    use SanitizedMessage::*;
    match tx.message() {
        Legacy(message) => {
            let msg = &message.message;
            trace!(
                "Processing Transaction:
header: {:#?}
account_keys: {:#?}
recent_blockhash: {}
message_hash: {}
instructions: {:?}
",
                msg.header,
                msg.account_keys,
                msg.recent_blockhash,
                tx.message_hash(),
                msg.instructions
            );
        }
        V0(msg) => trace!("Transaction: {:#?}", msg),
    }
}

fn log_execution_results(results: &[TransactionExecutionResult]) {
    if !log_enabled!(Trace) {
        return;
    }
    for result in results {
        match result {
            TransactionExecutionResult::Executed { details, .. } => {
                trace!("Executed: {:#?}", details);
            }
            TransactionExecutionResult::NotExecuted(err) => {
                trace!("NotExecuted: {:#?}", err);
            }
        }
    }
}

/// NOTE: a separate module for logging the blockhash is used
/// to in order to allow turning this off specifically
/// Example:
/// RUST_LOG=warn,magicblock=debug,magicblock_ledger=trace,magicblock_ledger::blockstore_processor::blockhash_log=off
mod blockhash_log {
    use super::*;
    pub(super) fn log_blockhash(slot: u64, blockhash: &Hash) {
        trace!("Slot {} Blockhash {}", slot, &blockhash);
    }
}
