use std::str::FromStr;

use log::{Level::Trace, *};
use magicblock_core::link::transactions::{
    SanitizeableTransaction, TransactionSchedulerHandle,
};
use num_format::{Locale, ToFormattedString};
use solana_clock::{Slot, UnixTimestamp};
use solana_hash::Hash;
use solana_transaction::versioned::VersionedTransaction;
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
            && slot.is_multiple_of(PROGRESS_REPORT_INTERVAL)
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
        // Transactions are stored in the ledger ordered by most recent to latest
        // such to replay them in the order they executed we need to reverse them
        for txn in block.transactions.into_iter().rev() {
            let signature = txn.signatures[0];
            // don't verify the signature, since we are operating on transaction
            // restored from ledger, the verification will fail
            let txn = txn.sanitize(false).map_err(|err| {
                LedgerError::BlockStoreProcessor(err.to_string())
            })?;
            let result =
                transaction_scheduler.replay(txn).await.map_err(|err| {
                    LedgerError::BlockStoreProcessor(err.to_string())
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
    Ok(slot + 1)
}

/// Processes the provided ledger updating the bank and returns the slot
/// at which the validator should continue processing (last processed slot + 1).
pub async fn process_ledger(
    ledger: &Ledger,
    full_process_starting_slot: Slot,
    transaction_scheduler: TransactionSchedulerHandle,
    max_age: u64,
) -> LedgerResult<u64> {
    // Since transactions may refer to blockhashes that were present when they
    // ran initially we ensure that they are present during replay as well
    let blockhashes_only_starting_slot =
        full_process_starting_slot.saturating_sub(max_age);
    debug!(
        "Loaded accounts into bank from storage replaying blockhashes from {} and transactions from {}",
        blockhashes_only_starting_slot, full_process_starting_slot
    );
    let slot = replay_blocks(
        IterBlocksParams {
            ledger,
            full_process_starting_slot,
            blockhashes_only_starting_slot,
        },
        transaction_scheduler,
    )
    .await?;
    Ok(slot)
}
