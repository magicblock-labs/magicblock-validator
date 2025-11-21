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

const PERIOD: u64 = 60;
const MAX_PERF_SAMPLES: usize = 720;

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
        count = count.min(MAX_PERF_SAMPLES);
        let index = PERF_SAMPLES.get_or_init(|| TreeIndex::default());
        let mut samples = Vec::with_capacity(count);
        for (slot, &sample) in index.iter(&Guard::new()).take(count) {
            let sample = RpcPerfSample {
                slot: slot.0,
                num_slots: sample.slots,
                num_transactions: sample.transactions,
                num_non_vote_transactions: None,
                sample_period_secs: PERIOD as u16,
            };
            samples.push(sample);
        }

        Ok(ResponsePayload::encode_no_context(&request.id, samples))
    }

    pub(crate) async fn run_perf_samples_collector(
        self: Arc<Self>,
        cancel: CancellationToken,
    ) {
        let mut interval = time::interval(Duration::from_secs(PERIOD));
        let mut last_slot = self.blocks.block_height();
        let mut last_count = TRANSACTION_COUNT.get();
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let count = TRANSACTION_COUNT.get();
                    let index = PERF_SAMPLES.get_or_init(|| TreeIndex::default());
                    let slot = self.blocks.block_height();
                    let sample = Sample {
                        slots: slot.saturating_sub(last_slot).max(1),
                        transactions: count.saturating_sub(last_count) as u64,
                    };
                    let _ = index.insert_async(Reverse(slot), sample).await;
                    if index.len() > MAX_PERF_SAMPLES {
                        const RANGE: u64 =  MAX_PERF_SAMPLES as u64 * 20;
                        let upper = Reverse(slot.saturating_sub(RANGE));
                        let lower = Reverse(upper.0.saturating_sub(RANGE));
                        index.remove_range_async(lower..upper).await;
                    }
                    last_slot = slot;
                    last_count = count;
                }
                _ = cancel.cancelled() => {
                    break;
                }
            }
        }
    }
}
