#![allow(dead_code)]
// NOTE: from core/src/banking_stage/leader_slot_timing_metrics.rs
use solana_metrics::datapoint_info;
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{saturating_add_assign, slot_history::Slot};

// NOTE: removed record_transactions_timings since they are poh related
#[derive(Default, Debug)]
pub struct LeaderExecuteAndCommitTimings {
    pub collect_balances_us: u64,
    pub load_execute_us: u64,
    pub freeze_lock_us: u64,
    pub last_blockhash_us: u64,
    pub record_us: u64,
    pub commit_us: u64,
    pub find_and_send_votes_us: u64,
    pub execute_timings: ExecuteTimings,
}

impl LeaderExecuteAndCommitTimings {
    pub fn accumulate(&mut self, other: &LeaderExecuteAndCommitTimings) {
        saturating_add_assign!(self.collect_balances_us, other.collect_balances_us);
        saturating_add_assign!(self.load_execute_us, other.load_execute_us);
        saturating_add_assign!(self.freeze_lock_us, other.freeze_lock_us);
        saturating_add_assign!(self.last_blockhash_us, other.last_blockhash_us);
        saturating_add_assign!(self.record_us, other.record_us);
        saturating_add_assign!(self.commit_us, other.commit_us);
        saturating_add_assign!(self.find_and_send_votes_us, other.find_and_send_votes_us);
        self.execute_timings.accumulate(&other.execute_timings);
    }

    pub fn report(&self, id: u32, slot: Slot) {
        datapoint_info!(
            "banking_stage-leader_slot_execute_and_commit_timings",
            ("id", id as i64, i64),
            ("slot", slot as i64, i64),
            ("collect_balances_us", self.collect_balances_us as i64, i64),
            ("load_execute_us", self.load_execute_us as i64, i64),
            ("freeze_lock_us", self.freeze_lock_us as i64, i64),
            ("last_blockhash_us", self.last_blockhash_us as i64, i64),
            ("record_us", self.record_us as i64, i64),
            ("commit_us", self.commit_us as i64, i64),
            (
                "find_and_send_votes_us",
                self.find_and_send_votes_us as i64,
                i64
            ),
        );
    }
}
