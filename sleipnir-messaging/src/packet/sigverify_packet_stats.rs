use serde::{Deserialize, Serialize};
use solana_frozen_abi_macro::AbiExample;
use solana_sdk::saturating_add_assign;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, AbiExample)]
pub struct SigverifyTracerPacketStats {
    pub total_removed_before_sigverify_stage: usize,
    pub total_tracer_packets_received_in_sigverify_stage: usize,
    pub total_tracer_packets_deduped: usize,
    pub total_excess_tracer_packets: usize,
    pub total_tracker_packets_passed_sigverify: usize,
}

impl SigverifyTracerPacketStats {
    pub fn is_default(&self) -> bool {
        *self == SigverifyTracerPacketStats::default()
    }

    pub fn aggregate(&mut self, other: &SigverifyTracerPacketStats) {
        saturating_add_assign!(
            self.total_removed_before_sigverify_stage,
            other.total_removed_before_sigverify_stage
        );
        saturating_add_assign!(
            self.total_tracer_packets_received_in_sigverify_stage,
            other.total_tracer_packets_received_in_sigverify_stage
        );
        saturating_add_assign!(
            self.total_tracer_packets_deduped,
            other.total_tracer_packets_deduped
        );
        saturating_add_assign!(
            self.total_excess_tracer_packets,
            other.total_excess_tracer_packets
        );
        saturating_add_assign!(
            self.total_tracker_packets_passed_sigverify,
            other.total_tracker_packets_passed_sigverify
        );
    }
}
