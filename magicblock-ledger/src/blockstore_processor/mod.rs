use std::{future::Future, pin::Pin, str::FromStr, sync::Arc};

use log::{Level::Trace, *};
use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::transactions::TransactionSchedulerHandle;
use num_format::{Locale, ToFormattedString};
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    hash::Hash,
    message::{SanitizedMessage, SimpleAddressLoader},
    transaction::{
        SanitizedTransaction, TransactionVerificationMode, VersionedTransaction,
    },
};
use solana_svm::transaction_commit_result::{
    TransactionCommitResult, TransactionCommitResultExtensions,
};
use solana_transaction_status::VersionedConfirmedBlock;

use crate::{
    errors::{LedgerError, LedgerResult},
    Ledger,
};

#[derive(Debug)]
struct PreparedBlock {
    slot: u64,
    blockhash: Hash,
    block_time: Option<UnixTimestamp>,
    transactions: Vec<VersionedTransaction>,
}

struct IterBlocksParams<'a> {
    ledger: &'a Ledger,
    full_process_starting_slot: Slot,
    blockhashes_only_starting_slot: Slot,
}

async fn replay_blocks(
    params: IterBlocksParams<'_>,
    transaction_scheduler: TransactionSchedulerHandle,
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
        let blockhash = Hash::from_str(&blockhash).map_err(|err| {
            LedgerError::BlockStoreProcessor(format!(
                "Failed to parse blockhash: {:?}",
                err
            ))
        })?;

        let block = PreparedBlock {
            slot,
            blockhash,
            block_time,
            transactions: successfull_txs,
        };

        let Some(timestamp) = block.block_time else {
            return Err(LedgerError::BlockStoreProcessor(format!(
                "Block has no timestamp, {block:?}",
            )));
        };
        ledger
            .latest_block()
            .store(block.slot, block.blockhash, timestamp);
        let mut block_txs = vec![];
        // Transactions are stored in the ledger ordered by most recent to latest
        // such to replay them in the order they executed we need to reverse them
        for tx in block.transactions.into_iter().rev() {
            let tx = tx.verify_and_hash_message().and_then(|hash| {
                SanitizedTransaction::try_create(
                    tx,
                    hash,
                    None,
                    SimpleAddressLoader::Disabled,
                    &Default::default(),
                )
            });

            match tx {
                Ok(tx) => block_txs.push(tx),
                Err(err) => {
                    return Err(LedgerError::BlockStoreProcessor(format!(
                        "Error processing transaction: {err:?}",
                    )));
                }
            };
        }
        for txn in block_txs {
            let signature = *txn.signature();
            let result =
                transaction_scheduler.replay(txn).await.ok_or_else(|| {
                    LedgerError::BlockStoreProcessor(
                        "Transaction Scheduler is not running".into(),
                    )
                });
            if !log_enabled!(Trace) {
                debug!("Result: {signature} - {result:?}");
            }
            if let Err(error) = result {
                return Err(LedgerError::BlockStoreProcessor(format!(
                    "Transaction '{signature}' could not be executed: {error}",
                )));
            }
        }
        slot += 1;
    }
    Ok(slot.max(1))
}

/// Processes the provided ledger updating the bank and returns the slot
/// at which the validator should continue processing (last processed slot + 1).
pub async fn process_ledger(
    ledger: &Ledger,
    accountsdb: &AccountsDb,
    transaction_scheduler: TransactionSchedulerHandle,
    max_age: u64,
) -> LedgerResult<u64> {
    // NOTE:
    // accountsdb was rolled back to max_slot (via ensure_at_most) during init
    // so the returned slot here is guaranteed to be equal or less than the
    // slot from `ledger.get_max_blockhash`
    let full_process_starting_slot = accountsdb.slot();

    // Since transactions may refer to blockhashes that were present when they
    // ran initially we ensure that they are present during replay as well
    let blockhashes_only_starting_slot = (full_process_starting_slot > max_age)
        .then_some(full_process_starting_slot - max_age)
        .unwrap_or_default();
    debug!(
        "Loaded accounts into bank from storage replaying blockhashes from {} and transactions from {}",
        blockhashes_only_starting_slot, full_process_starting_slot
    );
    replay_blocks(
        IterBlocksParams {
            ledger,
            full_process_starting_slot,
            blockhashes_only_starting_slot,
        },
        transaction_scheduler,
    )
    .await
}
