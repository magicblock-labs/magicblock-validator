use std::str::FromStr;

use log::{Level::Trace, *};
use magicblock_accounts_db::{
    utils::{all_accounts, StoredAccountMeta},
    AccountsPersister,
};
use magicblock_bank::bank::Bank;
use solana_sdk::{
    account::{Account, AccountSharedData, ReadableAccount},
    clock::{Slot, UnixTimestamp},
    hash::Hash,
    message::SanitizedMessage,
    transaction::{
        SanitizedTransaction, TransactionVerificationMode, VersionedTransaction,
    },
};
use solana_svm::transaction_processor::ExecutionRecordingConfig;
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
    Ok(slot)
}

fn hydrate_bank(bank: &Bank, max_slot: Slot) -> LedgerResult<(Slot, usize)> {
    let persister =
        AccountsPersister::new_with_paths(vec![bank.accounts_path.clone()]);
    let Some((storage, slot)) = persister.load_most_recent_store(max_slot)?
    else {
        return Ok((0, 0));
    };
    let storable_accounts =
        all_accounts(&storage, |acc_meta: StoredAccountMeta| {
            let acc = Account {
                lamports: acc_meta.lamports(),
                rent_epoch: acc_meta.rent_epoch(),
                owner: *acc_meta.owner(),
                executable: acc_meta.executable(),
                data: acc_meta.data().to_vec(),
            };
            (*acc_meta.pubkey(), AccountSharedData::from(acc))
        });
    let len = storable_accounts.len();
    bank.store_accounts(storable_accounts);
    Ok((slot, len))
}

/// Processes the provided ledger updating the bank and returns the slot
/// at which the validator should continue processing (last processed slot + 1).
pub fn process_ledger(ledger: &Ledger, bank: &Bank) -> LedgerResult<u64> {
    let (max_slot, _) = ledger.get_max_blockhash()?;
    let (full_process_starting_slot, len) = hydrate_bank(bank, max_slot)?;

    // Since transactions may refer to blockhashes that were present when they
    // ran initially we ensure that they are present during replay as well
    let blockhashes_only_starting_slot =
        if full_process_starting_slot > bank.max_age {
            full_process_starting_slot - bank.max_age
        } else {
            0
        };
    debug!(
        "Loaded {} accounts into bank from storage replaying blockhashes from {} and transactions from {}",
        len, blockhashes_only_starting_slot, full_process_starting_slot
    );
    iter_blocks(
        IterBlocksParams {
            ledger,
            full_process_starting_slot,
            blockhashes_only_starting_slot,
        },
        |prepared_block| {
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
                match bank.verify_transaction(
                    tx,
                    TransactionVerificationMode::HashOnly,
                ) {
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
                    let (results, _) = bank
                        .load_execute_and_commit_transactions(
                            &batch,
                            false,
                            ExecutionRecordingConfig::new_single_setting(true),
                            &mut timings,
                            None,
                        );
                    // TODO fix logging for TransactionCommitResult returned by load_execute_and_commit_transactions

                    //log_execution_results(&results.execution_results);
                    //for result in results.execution_results {
                    //    if let TransactionExecutionResult::NotExecuted(err) =
                    //        &result
                    //    {
                    //        // If we're on trace log level then we already logged this above
                    //        if !log_enabled!(Trace) {
                    //            debug!(
                    //                "Transactions: {:#?}",
                    //                batch.sanitized_transactions()
                    //            );
                    //            debug!("Result: {:#?}", result);
                    //        }
                    //        return Err(LedgerError::BlockStoreProcessor(
                    //            format!(
                    //        "Transaction {:?} could not be executed: {:?}",
                    //        result, err
                    //    ),
                    //        ));
                    //    }
                    //}
                }
            }
            Ok(())
        },
    )
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

// TODO fix logging for TransactionCommitResult returned by load_execute_and_commit_transactions
//
//fn log_execution_results(results: &[TransactionExecutionResult]) {
//    if !log_enabled!(Trace) {
//        return;
//    }
//    for result in results {
//        match result {
//            TransactionExecutionResult::Executed { details, .. } => {
//                trace!("Executed: {:#?}", details);
//            }
//            TransactionExecutionResult::NotExecuted(err) => {
//                trace!("NotExecuted: {:#?}", err);
//            }
//        }
//    }
//}

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
