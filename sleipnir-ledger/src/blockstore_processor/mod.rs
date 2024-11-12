use std::str::FromStr;

use crate::Ledger;
use log::*;
use sleipnir_bank::bank::{Bank, TransactionExecutionRecordingOpts};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::clock::UnixTimestamp;
use solana_sdk::hash::Hash;
use solana_sdk::transaction::{
    TransactionVerificationMode, VersionedTransaction,
};
use solana_transaction_status::VersionedConfirmedBlock;

#[derive(Debug)]
struct PreparedBlock {
    parent_slot: u64,
    slot: u64,
    previous_blockhash: Hash,
    blockhash: Hash,
    block_time: Option<UnixTimestamp>,
    block_height: Option<u64>,
    transactions: Vec<VersionedTransaction>,
}

fn iter_blocks_with_transaction(
    ledger: &Ledger,
    mut prepared_block_handler: impl FnMut(PreparedBlock),
) {
    let mut slot: u64 = 0;
    loop {
        let Ok(Some(block)) = ledger.get_block(slot) else {
            break;
        };
        let VersionedConfirmedBlock {
            blockhash,
            previous_blockhash,
            parent_slot,
            transactions,
            block_time,
            block_height,
            ..
        } = block;
        let successfull_txs = transactions
            .into_iter()
            .filter(|tx| tx.meta.status.is_ok())
            .map(|tx| tx.transaction)
            .collect::<Vec<_>>();
        // TODO: @@@ don't unwrap
        let previous_blockhash = Hash::from_str(&previous_blockhash).unwrap();
        let blockhash = Hash::from_str(&blockhash).unwrap();

        prepared_block_handler(PreparedBlock {
            parent_slot,
            slot,
            previous_blockhash,
            blockhash,
            block_time,
            block_height,
            transactions: successfull_txs,
        });
        slot += 1;
    }
}

pub fn process_ledger(ledger: &Ledger, bank: &Bank) {
    iter_blocks_with_transaction(ledger, |prepared_block| {
        let mut block_txs = vec![];
        let Some(timestamp) = prepared_block.block_time else {
            // TODO: @@@ most likely should bail here
            error!("Block has no timestamp, {:?}", prepared_block);
            return;
        };
        bank.warp_slot(
            prepared_block.slot,
            &prepared_block.previous_blockhash,
            &prepared_block.blockhash,
            timestamp as u64,
        );
        if !prepared_block.transactions.is_empty() {
            info!("Processing block: {:#?}", prepared_block);
        }
        for tx in prepared_block.transactions.into_iter().rev() {
            error!("Processing transaction: {:?}", tx);
            match bank
                .verify_transaction(tx, TransactionVerificationMode::HashOnly)
            {
                Ok(tx) => block_txs.push(tx),
                Err(err) => {
                    error!("Error processing transaction: {:?}", err);
                    // TODO: this is very bad we should probably shut things down
                    continue;
                }
            };
        }
        if !block_txs.is_empty() {
            // NOTE: ideally we would run all transactions in a single batch, but the
            // flawed account lock mechanism prevents this currently.
            // Until we revamp this transaction execution we execute each transaction
            // in its own batch.
            for tx in block_txs {
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
                info!("Results: {:#?}", results.execution_results);
            }
        }
    });
}
