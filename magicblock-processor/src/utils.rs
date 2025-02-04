// NOTE: copied from ledger/src/blockstore_processor.rs:106

use lazy_static::lazy_static;
use log::warn;
use magicblock_bank::transaction_batch::TransactionBatch;
use rayon::ThreadPool;
use solana_metrics::datapoint_error;
use solana_rayon_threadlimit::get_max_thread_count;
use solana_sdk::{signature::Signature, transaction::Result};
use solana_svm::transaction_commit_result::TransactionCommitResult;

// Includes transaction signature for unit-testing
pub fn get_first_error(
    batch: &TransactionBatch,
    commit_results: &[TransactionCommitResult],
) -> Option<(Result<()>, Signature)> {
    let mut first_err = None;
    for (commit_result, transaction) in
        commit_results.iter().zip(batch.sanitized_transactions())
    {
        if let Err(err) = commit_result {
            if first_err.is_none() {
                first_err = Some((Err(err.clone()), *transaction.signature()));
            }
            warn!(
                "Unexpected validator error: {:?}, transaction: {:?}",
                err, transaction
            );
            datapoint_error!(
                "validator_process_entry_error",
                (
                    "error",
                    format!("error: {err:?}, transaction: {transaction:?}"),
                    String
                )
            );
        }
    }
    first_err
}

// get_max_thread_count to match number of threads in the old code.
// see: https://github.com/solana-labs/solana/pull/24853
lazy_static! {
    pub(super) static ref PAR_THREAD_POOL: ThreadPool =
        rayon::ThreadPoolBuilder::new()
            .num_threads(get_max_thread_count())
            .thread_name(|i| format!("solBstoreProc{i:02}"))
            .build()
            .unwrap();
}

pub(super) fn first_err(results: &[Result<()>]) -> Result<()> {
    for r in results {
        if r.is_err() {
            return r.clone();
        }
    }
    Ok(())
}
