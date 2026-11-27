// The crank signer is deprecated for programs but still executed here.
#![allow(deprecated)]

use std::time::Duration as StdDuration;

use hydra_api::instruction::{CreateArgs, SchedMeta, ScheduledIx, ephemeral};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

/// Magic Program cranks used a u64 ID, Hydra cranks use a 32-byte seed.
/// The seed is `hash(authority, task_id)`, so each authority gets its own crank
/// namespace: a different authority scheduling the same `task_id` gets an
/// independent crank, and cancel/reschedule need no database lookup.
fn crank_seed(authority: &Pubkey, task_id: i64) -> [u8; 32] {
    solana_sha256_hasher::hashv(&[authority.as_ref(), &task_id.to_le_bytes()]).to_bytes()
}

/// Derives the deterministic hydra crank account address for a task.
pub fn crank_pubkey(authority: &Pubkey, task_id: i64) -> Pubkey {
    let seed = crank_seed(authority, task_id);
    ephemeral::find_crank_pda(&seed).0
}

/// Builds the hydra `Create` instruction embedding the task's instructions as
/// the scheduled crank payload, as scheduled.
///
/// `ScheduleTask` only accepts one signer: the authority's read-only crank
/// signer PDA. Its signer flag is kept, so the hydra cranker runs the crank
/// through the Crank program, which signs for it. Hydra accepts signer flags
/// on PDAs only. Tasks without a signer are replayed top-level as before.
#[allow(clippy::too_many_arguments)]
pub fn build_create_ix(
    sponsor: &Pubkey,
    authority: &Pubkey,
    task_id: i64,
    crank: Pubkey,
    start_slot: u64,
    interval_slots: u64,
    iterations: u64,
    instructions: &[Instruction],
) -> Instruction {
    let seed = crank_seed(authority, task_id);

    let metas_per_ix: Vec<Vec<SchedMeta>> = instructions
        .iter()
        .map(|ix| {
            ix.accounts
                .iter()
                .map(|acc| SchedMeta {
                    pubkey: acc.pubkey.to_bytes(),
                    is_writable: acc.is_writable,
                    is_signer: acc.is_signer,
                })
                .collect()
        })
        .collect();

    let scheduled: Vec<ScheduledIx<'_>> = instructions
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
        // The sponsor is also the cancel authority for the crank.
        authority: sponsor.to_bytes(),
        start_slot,
        interval_slots,
        remaining: iterations,
        priority_tip: 0,
        cu_limit: 0,
        scheduled: scheduled.as_slice(),
    };

    ephemeral::create(*sponsor, crank, &args)
}

pub fn is_valid_task_interval(interval: i64) -> bool {
    interval > 0 && interval < u32::MAX as i64
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
    use hydra_api::consts::{META_FLAG_SIGNER, META_FLAG_WRITABLE};
    use program::pda::crank_signer_pda;
    use solana_instruction::AccountMeta;

    use super::*;

    /// The flag byte `Create` stores for `meta` in its scheduled ix.
    fn stored_flag(create: &Instruction, meta: &Pubkey) -> u8 {
        let at = create
            .data
            .windows(32)
            .position(|window| window == meta.as_ref())
            .expect("meta in Create data");
        create.data[at - 1]
    }

    #[test]
    fn test_tasks_are_stored_as_scheduled_with_the_crank_signer_flag() {
        let sponsor = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let signer = crank_signer_pda(&authority);
        let account = Pubkey::new_unique();
        let ix = Instruction::new_with_bytes(
            Pubkey::new_unique(),
            &[1],
            vec![AccountMeta::new_readonly(signer, true), AccountMeta::new(account, false)],
        );
        let crank = crank_pubkey(&authority, 1);
        let create = build_create_ix(&sponsor, &authority, 1, crank, 0, 1, 0, &[ix]);
        assert_eq!(stored_flag(&create, &signer), META_FLAG_SIGNER);
        assert_eq!(stored_flag(&create, &account), META_FLAG_WRITABLE);
    }

    #[test]
    fn test_interval_millis_rounds_up_to_slots() {
        let slot = StdDuration::from_millis(50);
        assert_eq!(interval_slots(1, slot), 1);
        assert_eq!(interval_slots(50, slot), 1);
        assert_eq!(interval_slots(51, slot), 2);
        assert_eq!(interval_slots(100, slot), 2);
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
