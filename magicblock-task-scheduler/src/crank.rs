use std::time::Duration as StdDuration;

use hydra_api::{
    consts::{CRANK_HEADER_SIZE, SERIALIZED_META_SIZE},
    instruction::{ephemeral, CreateArgs, SchedMeta, ScheduledIx},
};
use magicblock_program::EPHEMERAL_RENT_PER_BYTE;
use solana_account::AccountSharedData;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

/// Derives the deterministic hydra crank account address for a task.
///
/// The seed is `hash(authority, task_id)`, so each authority gets its own crank
/// namespace: a different authority scheduling the same `task_id` gets an
/// independent crank, and cancel/reschedule need no database lookup.
pub fn crank_pubkey(authority: &Pubkey, task_id: i64) -> Pubkey {
    let seed = solana_sha256_hasher::hashv(&[
        authority.as_ref(),
        &task_id.to_le_bytes(),
    ])
    .to_bytes();
    ephemeral::find_crank_pda(&seed).0
}

/// Rent-exempt minimum for the crank account hydra will allocate for these
/// scheduled instructions. Mirrors hydra's on-chain size accounting
/// (`CRANK_HEADER_SIZE + region_len`, where each scheduled ix serializes to
/// `2 + metas + program_id + 2 + data`).
pub fn crank_rent_floor(instructions: &[Instruction]) -> u64 {
    let region_len: usize = instructions
        .iter()
        .map(|ix| {
            2 + ix.accounts.len() * SERIALIZED_META_SIZE
                + 32
                + 2
                + ix.data.len()
        })
        .sum::<usize>()
        + CRANK_HEADER_SIZE;
    let total_size =
        (region_len + AccountSharedData::ACCOUNT_STATIC_SIZE as usize) as u64;
    total_size * EPHEMERAL_RENT_PER_BYTE
}

/// Builds the hydra `Create` instruction embedding the task's instructions as
/// the scheduled crank payload. Account signer flags are intentionally dropped:
/// hydra rejects scheduled instructions that declare signers.
#[allow(clippy::too_many_arguments)]
pub fn build_create_ix(
    faucet: &Pubkey,
    authority: &Pubkey,
    task_id: i64,
    crank: Pubkey,
    start_slot: u64,
    interval_slots: u64,
    iterations: u64,
    instructions: &[Instruction],
) -> Instruction {
    let seed = solana_sha256_hasher::hashv(&[
        authority.as_ref(),
        &task_id.to_le_bytes(),
    ])
    .to_bytes();

    let metas_per_ix: Vec<Vec<SchedMeta>> = instructions
        .iter()
        .map(|ix| {
            ix.accounts
                .iter()
                .map(|acc| SchedMeta {
                    pubkey: acc.pubkey.to_bytes(),
                    is_writable: acc.is_writable,
                })
                .collect()
        })
        .collect();

    let scheduled: Vec<ScheduledIx> = instructions
        .iter()
        .zip(metas_per_ix.iter())
        .map(|(ix, metas)| ScheduledIx {
            program_id: ix.program_id.to_bytes(),
            metas: metas.as_slice(),
            data: ix.data.as_slice(),
        })
        .collect();

    let args = CreateArgs {
        seed,
        // The faucet is the sponsor and the cancel authority for the crank.
        authority: faucet.to_bytes(),
        start_slot,
        interval_slots,
        remaining: iterations,
        priority_tip: 0,
        cu_limit: 0,
        scheduled: scheduled.as_slice(),
    };

    ephemeral::create(*faucet, crank, &args)
}

pub fn is_valid_task_interval(interval: i64) -> bool {
    interval > 0 && interval < u32::MAX as i64
}

/// Computes the hydra start slot for a task migrated from the legacy scheduler.
/// If the legacy task is overdue or has never run, it remains due immediately.
pub fn legacy_start_slot(
    last_execution_millis: i64,
    interval_millis: i64,
    current_millis: i64,
    current_slot: u64,
    slot_interval: StdDuration,
) -> u64 {
    if last_execution_millis <= 0 {
        return current_slot;
    }

    let next_execution_millis =
        last_execution_millis.saturating_add(interval_millis.max(0));
    let remaining_millis = next_execution_millis.saturating_sub(current_millis);
    if remaining_millis == 0 {
        return current_slot;
    }

    current_slot.saturating_add(interval_slots(remaining_millis, slot_interval))
}

/// Converts a millisecond execution interval into a slot count (rounding up,
/// with a one-slot minimum) for hydra's slot-based cadence.
pub fn interval_slots(interval_millis: i64, slot_interval: StdDuration) -> u64 {
    let slot_millis = (slot_interval.as_millis() as i64).max(1);
    let interval_millis = interval_millis.max(0);
    // Ceiling division without the unstable `i64::div_ceil`.
    let slots = (interval_millis + slot_millis - 1) / slot_millis;
    slots.max(1) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interval_millis_rounds_up_to_slots() {
        let slot = StdDuration::from_millis(50);
        assert_eq!(interval_slots(1, slot), 1);
        assert_eq!(interval_slots(50, slot), 1);
        assert_eq!(interval_slots(51, slot), 2);
        assert_eq!(interval_slots(100, slot), 2);
    }

    #[test]
    fn test_legacy_start_slot_preserves_remaining_delay() {
        let slot_interval = StdDuration::from_millis(1_000);

        assert_eq!(
            legacy_start_slot(10_000, 30_000, 25_000, 100, slot_interval),
            115
        );
        assert_eq!(
            legacy_start_slot(10_000, 30_000, 40_000, 100, slot_interval),
            100
        );
        assert_eq!(
            legacy_start_slot(0, 30_000, 25_000, 100, slot_interval),
            100
        );
    }

    #[test]
    fn test_crank_pubkey_namespaced_by_authority_and_id() {
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        // Deterministic.
        assert_eq!(crank_pubkey(&a, 1), crank_pubkey(&a, 1));
        // Different task id -> different crank.
        assert_ne!(crank_pubkey(&a, 1), crank_pubkey(&a, 2));
        // Different authority, same id -> different crank (per-authority namespace).
        assert_ne!(crank_pubkey(&a, 1), crank_pubkey(&b, 1));
    }
}
