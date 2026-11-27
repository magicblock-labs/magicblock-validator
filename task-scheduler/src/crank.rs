use std::time::Duration as StdDuration;

use hydra_api::{
    MAX_ACCOUNTS, MAX_DATA_LEN,
    instruction::{CreateArgs, SchedMeta, ScheduledIx, ephemeral},
};
use program::crank::{execute_crank_instruction, task_crank_seed as crank_seed, uses_crank_signer};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::errors::{TaskSchedulerError, TaskSchedulerResult};

/// Derives the deterministic hydra crank account address for a task.
pub fn crank_pubkey(authority: &Pubkey, task_id: i64) -> Pubkey {
    let seed = crank_seed(authority, task_id);
    ephemeral::find_crank_pda(&seed).0
}

/// Returns the instructions hydra stores and replays for a task.
///
/// Tasks that do not use their crank signer run as-is, as top-level siblings of
/// hydra's `Trigger`. Hydra cannot sign for the crank signer PDA, so a task
/// that does is wrapped into a single Crank program `ExecuteCrank`, which
/// invokes the task's instructions with that signer.
pub fn scheduled_instructions(
    authority: &Pubkey,
    task_id: i64,
    crank: Pubkey,
    instructions: &[Instruction],
) -> TaskSchedulerResult<Vec<Instruction>> {
    let scheduled = if uses_crank_signer(authority, instructions) {
        vec![execute_crank_instruction(crank, *authority, task_id, instructions.to_vec())]
    } else {
        instructions.to_vec()
    };
    // Surface hydra's per-instruction limits here rather than as an opaque
    // `Create` failure.
    for ix in &scheduled {
        if ix.accounts.len() > MAX_ACCOUNTS || ix.data.len() > MAX_DATA_LEN {
            return Err(TaskSchedulerError::CrankTooLarge {
                task_id,
                accounts: ix.accounts.len(),
                max_accounts: MAX_ACCOUNTS,
                data_len: ix.data.len(),
                max_data_len: MAX_DATA_LEN,
            });
        }
    }
    Ok(scheduled)
}

/// Builds the hydra `Create` instruction embedding the task's
/// [`scheduled_instructions`] as the crank payload. Account signer flags are
/// intentionally dropped: hydra rejects scheduled instructions that declare
/// signers.
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
) -> TaskSchedulerResult<Instruction> {
    let seed = crank_seed(authority, task_id);
    let instructions = scheduled_instructions(authority, task_id, crank, instructions)?;

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

    Ok(ephemeral::create(*sponsor, crank, &args))
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
    use program::{CRANK_PROGRAM_ID, pda::crank_signer_pda};
    use solana_instruction::AccountMeta;

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
    fn test_tasks_without_crank_signer_run_unwrapped() {
        let authority = Pubkey::new_unique();
        let ix = Instruction::new_with_bytes(
            Pubkey::new_unique(),
            &[1],
            vec![AccountMeta::new(Pubkey::new_unique(), false)],
        );
        let crank = crank_pubkey(&authority, 1);
        let scheduled =
            scheduled_instructions(&authority, 1, crank, std::slice::from_ref(&ix)).unwrap();
        assert_eq!(scheduled, vec![ix]);
    }

    #[test]
    fn test_tasks_with_crank_signer_run_through_the_crank_program() {
        let authority = Pubkey::new_unique();
        let ixs = vec![Instruction::new_with_bytes(
            Pubkey::new_unique(),
            &[1],
            vec![AccountMeta::new_readonly(crank_signer_pda(&authority), true)],
        )];
        let crank = crank_pubkey(&authority, 1);
        let scheduled = scheduled_instructions(&authority, 1, crank, &ixs).unwrap();
        assert_eq!(
            scheduled,
            vec![execute_crank_instruction(crank, authority, 1, ixs)]
        );
        assert_eq!(scheduled[0].program_id, CRANK_PROGRAM_ID);
        // Hydra stores no signer metas.
        assert!(scheduled[0].accounts.iter().all(|meta| !meta.is_signer));
    }

    #[test]
    fn test_oversized_wrapped_task_is_rejected() {
        let authority = Pubkey::new_unique();
        let ixs = vec![Instruction::new_with_bytes(
            Pubkey::new_unique(),
            &[0; MAX_DATA_LEN],
            vec![AccountMeta::new_readonly(crank_signer_pda(&authority), true)],
        )];
        let crank = crank_pubkey(&authority, 1);
        assert!(matches!(
            scheduled_instructions(&authority, 1, crank, &ixs),
            Err(TaskSchedulerError::CrankTooLarge { .. })
        ));
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
