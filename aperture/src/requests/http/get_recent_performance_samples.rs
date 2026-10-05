use std::{
    sync::{Arc, PoisonError},
    time::Duration,
};

use solana_rpc_client_api::response::RpcPerfSample;
use tokio::time;
use tokio_util::sync::CancellationToken;

use super::{HandlerResult, RpcHandlers};
use crate::requests::{JsonHttpRequest as JsonRequest, payload::ResponsePayload};

const PERIOD_SECS: u64 = 60;
/// Twelve hours of minute samples, independent of block production rate.
const MAX_PERF_SAMPLES: usize = 720;

impl RpcHandlers {
    pub(crate) fn get_recent_performance_samples(&self, request: &JsonRequest) -> HandlerResult {
        let count = request.required::<usize>(0)?.min(MAX_PERF_SAMPLES);
        let samples = self.samples.lock().unwrap_or_else(PoisonError::into_inner);
        ResponsePayload::encode_no_context(
            &request.id,
            samples.iter().take(count).collect::<Vec<_>>(),
        )
    }

    /// Retains at most 720 samples, replacing the newest when its slot is unchanged.
    pub(crate) async fn run_perf_samples_collector(self: Arc<Self>, cancel: CancellationToken) {
        let mut interval = time::interval(Duration::from_secs(PERIOD_SECS));
        let mut last_slot = self.engine.blocks().latest().slot;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let slot = self.engine.blocks().latest().slot;
                    let sample = RpcPerfSample {
                        slot,
                        num_slots: slot.saturating_sub(last_slot).max(1),
                        num_transactions: 0,
                        num_non_vote_transactions: None,
                        sample_period_secs: PERIOD_SECS as u16,
                    };
                    let mut samples = self.samples.lock().unwrap_or_else(PoisonError::into_inner);
                    if samples.front().is_some_and(|sample| sample.slot == slot) {
                        samples.pop_front();
                    }
                    samples.push_front(sample);
                    samples.truncate(MAX_PERF_SAMPLES);
                    last_slot = slot;
                }
                _ = cancel.cancelled() => break,
            }
        }
    }
}
