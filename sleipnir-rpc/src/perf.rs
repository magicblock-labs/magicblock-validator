use sleipnir_ledger::PerfSample;
use solana_rpc_client_api::response::RpcPerfSample;
use solana_sdk::clock::Slot;

pub fn rpc_perf_sample_from(
    (slot, perf_sample): (Slot, PerfSample),
) -> RpcPerfSample {
    RpcPerfSample {
        slot,
        num_transactions: perf_sample.num_transactions,
        num_slots: perf_sample.num_slots,
        sample_period_secs: perf_sample.sample_period_secs,
        num_non_vote_transactions: Some(perf_sample.num_non_vote_transactions),
    }
}
