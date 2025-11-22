use std::{
    cmp::Reverse,
    sync::{Arc, OnceLock},
    time::Duration,
};

use magicblock_metrics::metrics::TRANSACTION_COUNT;
use scc::{ebr::Guard, TreeIndex};
use solana_rpc_client_api::response::RpcPerfSample;
use tokio::time;
use tokio_util::sync::CancellationToken;

use super::prelude::*;

/// 60 seconds per sample
const PERIOD_SECS: u64 = 60;

/// Keep 12 hours of history (720 minutes)
const MAX_PERF_SAMPLES: usize = 720;

/// Estimated blocks per minute:
/// Nominal = 1200 (20 blocks/sec * 60s).
/// We use 1500 (25% buffer) to ensure the cleanup
/// logic never accidentally prunes valid history
const ESTIMATED_SLOTS_PER_SAMPLE: u64 = 1500;

static PERF_SAMPLES: OnceLock<TreeIndex<Reverse<Slot>, Sample>> =
    OnceLock::new();

#[derive(Clone, Copy)]
struct Sample {
    transactions: u64,
    slots: u64,
}

impl HttpDispatcher {
    pub(crate) fn get_recent_performance_samples(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let count = parse_params!(request.params()?, usize);
        let mut count: usize = some_or_err!(count);

        // Cap request at max history size (12h)
        count = count.min(MAX_PERF_SAMPLES);

        let index = PERF_SAMPLES.get_or_init(TreeIndex::default);
        let mut samples = Vec::with_capacity(count);

        // Index is keyed by Reverse(Slot), so iter() yields Newest -> Oldest
        for (slot, &sample) in index.iter(&Guard::new()).take(count) {
            samples.push(RpcPerfSample {
                slot: slot.0,
                num_slots: sample.slots,
                num_transactions: sample.transactions,
                num_non_vote_transactions: None,
                sample_period_secs: PERIOD_SECS as u16,
            });
        }

        Ok(ResponsePayload::encode_no_context(&request.id, samples))
    }

    pub(crate) async fn run_perf_samples_collector(
        self: Arc<Self>,
        cancel: CancellationToken,
    ) {
        let mut interval = time::interval(Duration::from_secs(PERIOD_SECS));

        let mut last_slot = self.blocks.block_height();
        let mut last_tx_count = TRANSACTION_COUNT.get();

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Capture current state
                    let current_slot = self.blocks.block_height();
                    let current_tx_count = TRANSACTION_COUNT.get();

                    // Calculate Deltas (Activity within the last 60s)
                    let slots_delta = current_slot.saturating_sub(last_slot).max(1);
                    let tx_delta = current_tx_count.saturating_sub(last_tx_count);

                    let index = PERF_SAMPLES.get_or_init(TreeIndex::default);
                    let sample = Sample {
                        slots: slots_delta,
                        transactions: tx_delta,
                    };
                    let _ = index.insert_async(Reverse(current_slot), sample).await;

                    // Prune old history
                    if index.len() > MAX_PERF_SAMPLES {
                        // Calculate cutoff: 720 samples * 1500 blocks = ~1.08M blocks history
                        let retention_range = MAX_PERF_SAMPLES as u64 * ESTIMATED_SLOTS_PER_SAMPLE;
                        let cutoff_slot = current_slot.saturating_sub(retention_range);

                        // Remove everything OLDER than the cutoff.
                        // In Reverse(), "Older" (Smaller Slot) == "Greater Value".
                        // RangeFrom (cutoff..) removes the tail of the tree.
                        index.remove_range_async(Reverse(cutoff_slot)..).await;
                    }

                    // Update baseline for next tick
                    last_slot = current_slot;
                    last_tx_count = current_tx_count;
                }
                _ = cancel.cancelled() => {
                    break;
                }
            }
        }
    }
}
