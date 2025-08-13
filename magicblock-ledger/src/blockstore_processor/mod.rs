use std::{str::FromStr, sync::Arc};

use log::{Level::Trace, *};
use magicblock_accounts_db::AccountsDb;
use num_format::{Locale, ToFormattedString};
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    hash::Hash,
    message::SanitizedMessage,
    transaction::{
        SanitizedTransaction, TransactionVerificationMode, VersionedTransaction,
    },
};
use solana_svm::{
    transaction_commit_result::{
        TransactionCommitResult, TransactionCommitResultExtensions,
    },
    transaction_processor::ExecutionRecordingConfig,
};
use solana_timings::ExecuteTimings;
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

struct IterBlocksParams<'a> {
    ledger: &'a Ledger,
    full_process_starting_slot: Slot,
    blockhashes_only_starting_slot: Slot,
}

fn iter_blocks(
    params: IterBlocksParams,
    mut prepared_block_handler: impl FnMut(PreparedBlock) -> LedgerResult<()>,
) -> LedgerResult<u64> {
    let IterBlocksParams {
        ledger,
        full_process_starting_slot,
        blockhashes_only_starting_slot,
    } = params;
    let mut slot: u64 = blockhashes_only_starting_slot;

    let max_slot = if log::log_enabled!(Level::Info) {
        ledger
            .get_max_blockhash()?
            .0
            .to_formatted_string(&Locale::en)
    } else {
        "N/A".to_string()
    };
    const PROGRESS_REPORT_INTERVAL: u64 = 100;
    loop {
        let Ok(Some(block)) = ledger.get_block(slot) else {
            break;
        };
        if log::log_enabled!(Level::Info)
            && slot % PROGRESS_REPORT_INTERVAL == 0
        {
            info!(
                "Processing block: {}/{}",
                slot.to_formatted_string(&Locale::en),
                max_slot
            );
        }

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

        // We skip all transactions until we reach the slot at which we should
        // start processing them. Up to that slot we only process blockhashes.
        let successfull_txs = if slot >= full_process_starting_slot {
            // We only re-run transactions that succeeded since errored transactions
            // don't update any state
            transactions
                .into_iter()
                .filter(|tx| tx.meta.status.is_ok())
                .map(|tx| tx.transaction)
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
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
    Ok(slot.max(1))
}

/// Processes the provided ledger updating the bank and returns the slot
/// at which the validator should continue processing (last processed slot + 1).
pub fn process_ledger(
    ledger: &Ledger,
    accountsdb: &AccountsDb,
    max_age: u64,
) -> LedgerResult<u64> {
    // // NOTE:
    // // bank.adb was rolled back to max_slot (via ensure_at_most) in magicblock-bank/src/bank.rs
    // // `Bank::new` method, so the returned slot here is guaranteed to be equal or less than the
    // // slot from `ledger.get_max_blockhash`
    // let full_process_starting_slot = accountsdb.slot();

    // // Since transactions may refer to blockhashes that were present when they
    // // ran initially we ensure that they are present during replay as well
    // let blockhashes_only_starting_slot = (full_process_starting_slot > max_age)
    //     .then_some(full_process_starting_slot - max_age)
    //     .unwrap_or_default();
    // debug!(
    //     "Loaded accounts into bank from storage replaying blockhashes from {} and transactions from {}",
    //     blockhashes_only_starting_slot, full_process_starting_slot
    // );
    // iter_blocks(
    //     IterBlocksParams {
    //         ledger,
    //         full_process_starting_slot,
    //         blockhashes_only_starting_slot,
    //     },
    //     |prepared_block| {
    //         let mut block_txs = vec![];
    //         let Some(timestamp) = prepared_block.block_time else {
    //             return Err(LedgerError::BlockStoreProcessor(format!(
    //                 "Block has no timestamp, {:?}",
    //                 prepared_block
    //             )));
    //         };
    //         blockhash_log::log_blockhash(
    //             prepared_block.slot,
    //             &prepared_block.blockhash,
    //         );
    //         bank.replay_slot(
    //             prepared_block.slot,
    //             &prepared_block.previous_blockhash,
    //             &prepared_block.blockhash,
    //             timestamp as u64,
    //         );

    //         // Transactions are stored in the ledger ordered by most recent to latest
    //         // such to replay them in the order they executed we need to reverse them
    //         for tx in prepared_block.transactions.into_iter().rev() {
    //             match bank.verify_transaction(
    //                 tx,
    //                 TransactionVerificationMode::HashOnly,
    //             ) {
    //                 Ok(tx) => block_txs.push(tx),
    //                 Err(err) => {
    //                     return Err(LedgerError::BlockStoreProcessor(format!(
    //                         "Error processing transaction: {:?}",
    //                         err
    //                     )));
    //                 }
    //             };
    //         }
    //         if !block_txs.is_empty() {
    //             // NOTE: ideally we would run all transactions in a single batch, but the
    //             // flawed account lock mechanism prevents this currently.
    //             // Until we revamp this transaction execution we execute each transaction
    //             // in its own batch.
    //             for tx in block_txs {
    //                 log_sanitized_transaction(&tx);

    //                 let mut timings = ExecuteTimings::default();
    //                 let signature = *tx.signature();
    //                 let batch = [tx];
    //                 let batch = bank.prepare_sanitized_batch(&batch);
    //                 let (results, _) = bank
    //                     .load_execute_and_commit_transactions(
    //                         &batch,
    //                         false,
    //                         ExecutionRecordingConfig::new_single_setting(true),
    //                         &mut timings,
    //                         None,
    //                     );

    //                 log_execution_results(&results);
    //                 for result in results {
    //                     if !result.was_executed_successfully() {
    //                         // If we're on trace log level then we already logged this above
    //                         if !log_enabled!(Trace) {
    //                             debug!(
    //                                 "Transactions: {:#?}",
    //                                 batch.sanitized_transactions()
    //                             );
    //                             debug!("Result: {:#?}", result);
    //                         }
    //                         let err = match &result {
    //                             Ok(tx) => match &tx.status {
    //                                 Ok(_) => None,
    //                                 Err(err) => Some(err),
    //                             },
    //                             Err(err) => Some(err),
    //                         };
    //                         return Err(LedgerError::BlockStoreProcessor(
    //                             format!(
    //                                 "Transaction '{}', {:?} could not be executed: {:?}",
    //                                 signature, result, err
    //                             ),
    //                         ));
    //                     }
    //                 }
    //             }
    //         }
    //         Ok(())
    //     },
    // )
    Ok(0)
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

fn log_execution_results(results: &[TransactionCommitResult]) {
    if !log_enabled!(Trace) {
        return;
    }
    for result in results {
        match result {
            Ok(tx) => {
                if result.was_executed_successfully() {
                    trace!(
                        "Executed: (fees: {:#?}, loaded accounts; {:#?})",
                        tx.fee_details,
                        tx.loaded_account_stats
                    );
                } else {
                    trace!("NotExecuted: {:#?}", tx.status);
                }
            }
            Err(err) => {
                trace!("Failed: {:#?}", err);
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
