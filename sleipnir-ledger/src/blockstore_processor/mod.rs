use log::*;
use sleipnir_bank::bank::{Bank, TransactionExecutionRecordingOpts};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::transaction::{
    TransactionVerificationMode, VersionedTransaction,
};

use crate::Ledger;

struct BlockWithTransactions {
    slot: u64,
    transactions: Vec<VersionedTransaction>,
}

fn iter_blocks_with_transaction(
    ledger: &Ledger,
    mut block_with_tx_handler: impl FnMut(BlockWithTransactions),
) {
    let mut slot: u64 = 0;
    loop {
        let Ok(Some(block)) = ledger.get_block(slot) else {
            break;
        };
        if !block.transactions.is_empty() {
            let successfull_txs = block
                .transactions
                .into_iter()
                .filter(|tx| tx.meta.status.is_ok())
                .map(|tx| tx.transaction)
                .collect::<Vec<_>>();
            if !successfull_txs.is_empty() {
                block_with_tx_handler(BlockWithTransactions {
                    slot,
                    transactions: successfull_txs,
                });
            }
        }
        slot += 1;
    }
}

pub fn process_ledger(ledger: &Ledger, bank: &Bank) {
    error!("-------------- PRocessing");
    iter_blocks_with_transaction(ledger, |block_with_tx| {
        let mut block_txs = vec![];
        for tx in block_with_tx.transactions.into_iter() {
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
        let batch = bank.prepare_sanitized_batch(&block_txs);

        let mut timings = ExecuteTimings::default();
        let (results, _) = bank.load_execute_and_commit_transactions(
            &batch,
            false,
            TransactionExecutionRecordingOpts::recording_logs(),
            &mut timings,
            None,
        );
        info!("Results: {:#?}", results.execution_results);
    });
}
