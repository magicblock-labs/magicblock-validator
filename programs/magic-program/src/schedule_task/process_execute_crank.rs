use std::mem::offset_of;

use hydra_api::{CRANK_HEADER_SIZE, consts::ix::TRIGGER, ephemeral, state::Crank};
use magic_program_api::{CRANK_PROGRAM_ID, pda::crank_signer_pda};
use solana_account::ReadableAccount;
use solana_instruction::{Instruction, TRANSACTION_LEVEL_STACK_HEIGHT, error::InstructionError};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::instructions as instructions_sysvar;

use crate::{
    crank::{
        EXECUTE_CRANK_CRANK_IDX, EXECUTE_CRANK_SIGNER_IDX, EXECUTE_CRANK_SYSVAR_IDX,
        task_crank_seed,
    },
    schedule_task::validate_cranks_instructions,
    utils::accounts::{get_instruction_account_with_idx, get_instruction_pubkey_with_idx},
    validator::authority as validator_authority,
};

const HYDRA_PROGRAM_ID: Pubkey = Pubkey::new_from_array(ephemeral::ID.to_bytes());
const CRANK_AUTHORITY: usize = offset_of!(Crank, authority);
const CRANK_SEED: usize = offset_of!(Crank, seed);
const CRANK_AUTHORITY_SIGNER: usize = offset_of!(Crank, authority_signer);
const SERIALIZED_META_SIZE: usize = 33;

/// Executes a task's instructions with its authority's crank signer.
///
/// Anyone may submit the hydra `Trigger` that precedes this instruction, so
/// authorization comes from the crank rather than from the transaction signer:
/// 1. `Trigger` of crank `C` sits directly before this instruction, so hydra
///    verified this instruction against `C`'s stored template.
/// 2. `C` is hydra-owned and was created and signed for by the validator
///    authority, so the template was written by the task scheduler.
/// 3. `C`'s seed is `sha256(authority, task_id)`, so the template belongs to
///    the task that `authority` itself scheduled.
pub(crate) fn process_execute_crank(
    invoke_context: &mut InvokeContext<'_, '_>,
    authority: &Pubkey,
    task_id: i64,
    instructions: Vec<Instruction>,
) -> Result<(), InstructionError> {
    let crank_signer = crank_signer_pda(authority);
    validate(invoke_context, authority, task_id, &crank_signer)?;
    // Already validated when the task was scheduled; repeated so a crank can
    // never carry what `ScheduleTask` would have rejected.
    validate_cranks_instructions(invoke_context, authority, &instructions)?;

    let len = instructions.len();
    for ix in instructions {
        invoke_context.native_invoke_as(CRANK_PROGRAM_ID, ix, &[crank_signer])?;
    }
    ic_msg!(invoke_context, "Executed crank with {} instructions", len);
    Ok(())
}

fn validate(
    invoke_context: &InvokeContext<'_, '_>,
    authority: &Pubkey,
    task_id: i64,
    crank_signer: &Pubkey,
) -> Result<(), InstructionError> {
    let tc = &*invoke_context.transaction_context;
    let ix_ctx = tc.get_current_instruction_context()?;

    if ix_ctx.get_program_key()? != &CRANK_PROGRAM_ID {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: crank executor program account not found"
        );
        return Err(InstructionError::UnsupportedProgramId);
    }
    // The instructions sysvar describes top-level instructions only.
    if invoke_context.get_stack_height() != TRANSACTION_LEVEL_STACK_HEIGHT {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: must be a top-level instruction"
        );
        return Err(InstructionError::IncorrectProgramId);
    }
    if (ix_ctx.get_number_of_instruction_accounts() as usize) <= EXECUTE_CRANK_SYSVAR_IDX as usize {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: missing crank, crank signer or sysvar"
        );
        return Err(InstructionError::MissingAccount);
    }

    let signer = get_instruction_pubkey_with_idx(tc, EXECUTE_CRANK_SIGNER_IDX)?;
    if signer != crank_signer {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: crank signer pubkey {} is not the expected Crank signer",
            signer
        );
        return Err(InstructionError::InvalidSeeds);
    }

    let crank = *get_instruction_pubkey_with_idx(tc, EXECUTE_CRANK_CRANK_IDX)?;
    {
        let account = get_instruction_account_with_idx(tc, EXECUTE_CRANK_CRANK_IDX)?;
        let account = account.borrow()?;
        if account.owner() != &HYDRA_PROGRAM_ID {
            ic_msg!(
                invoke_context,
                "ExecuteCrank ERR: crank {} is not owned by hydra",
                crank
            );
            return Err(InstructionError::InvalidAccountOwner);
        }
        let data = account.data();
        if data.len() < CRANK_HEADER_SIZE {
            return Err(InstructionError::InvalidAccountData);
        }
        let created_by_validator = data[CRANK_AUTHORITY..CRANK_AUTHORITY + 32]
            == validator_authority().to_bytes()
            && data[CRANK_AUTHORITY_SIGNER] == 1;
        if !created_by_validator {
            ic_msg!(
                invoke_context,
                "ExecuteCrank ERR: crank {} was not created by the validator",
                crank
            );
            return Err(InstructionError::IncorrectAuthority);
        }
        if data[CRANK_SEED..CRANK_SEED + 32] != task_crank_seed(authority, task_id) {
            ic_msg!(
                invoke_context,
                "ExecuteCrank ERR: crank {} does not belong to task {} of {}",
                crank,
                task_id,
                authority
            );
            return Err(InstructionError::InvalidSeeds);
        }
    }

    let sysvar = get_instruction_account_with_idx(tc, EXECUTE_CRANK_SYSVAR_IDX)?;
    if get_instruction_pubkey_with_idx(tc, EXECUTE_CRANK_SYSVAR_IDX)? != &instructions_sysvar::ID {
        return Err(InstructionError::UnsupportedSysvar);
    }
    let sysvar = sysvar.borrow()?;
    if !follows_trigger_of(sysvar.data(), &crank) {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: must directly follow hydra Trigger of crank {}",
            crank
        );
        return Err(InstructionError::InvalidInstructionData);
    }
    Ok(())
}

/// Whether the top-level instruction right before the current one is hydra's
/// `Trigger` of `crank` (`Trigger` accounts: `[crank, cranker, sysvar]`).
///
/// Parses the instructions sysvar: `[num_ix u16][offset u16; num_ix]` then per
/// instruction `[num_accounts u16][(flags u8, pubkey); num_accounts]
/// [program_id][data_len u16][data]`, with the current index in the last two
/// bytes. Any out-of-bounds read is a mismatch.
fn follows_trigger_of(sysvar: &[u8], crank: &Pubkey) -> bool {
    fn u16_at(data: &[u8], at: usize) -> Option<usize> {
        let bytes = data.get(at..at.checked_add(2)?)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]) as usize)
    }
    fn pubkey_at(data: &[u8], at: usize) -> Option<&[u8]> {
        data.get(at..at.checked_add(32)?)
    }
    let check = || -> Option<bool> {
        let num_ix = u16_at(sysvar, 0)?;
        let current = u16_at(sysvar, sysvar.len().checked_sub(2)?)?;
        let previous = current.checked_sub(1)?;
        if current >= num_ix {
            return None;
        }
        let start = u16_at(sysvar, 2 + 2 * previous)?;
        let num_accounts = u16_at(sysvar, start)?;
        if num_accounts == 0 {
            return Some(false);
        }
        // First meta: skip its flag byte.
        let first_account = pubkey_at(sysvar, start + 2 + 1)?;
        let program_at = start + 2 + num_accounts * SERIALIZED_META_SIZE;
        let program_id = pubkey_at(sysvar, program_at)?;
        let data_len = u16_at(sysvar, program_at + 32)?;
        let data = sysvar.get(program_at + 34..program_at + 34 + data_len)?;
        Some(
            program_id == HYDRA_PROGRAM_ID.as_ref()
                && first_account == crank.as_ref()
                && data.first() == Some(&TRIGGER),
        )
    };
    check().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use solana_instruction::{AccountMeta, BorrowedAccountMeta, BorrowedInstruction};
    use solana_instructions_sysvar::construct_instructions_data;

    use super::*;

    fn sysvar_with(instructions: &[Instruction], current: u16) -> Vec<u8> {
        let borrowed: Vec<BorrowedInstruction<'_>> = instructions
            .iter()
            .map(|ix| BorrowedInstruction {
                program_id: &ix.program_id,
                accounts: ix
                    .accounts
                    .iter()
                    .map(|meta| BorrowedAccountMeta {
                        pubkey: &meta.pubkey,
                        is_signer: meta.is_signer,
                        is_writable: meta.is_writable,
                    })
                    .collect(),
                data: &ix.data,
            })
            .collect();
        let mut data = construct_instructions_data(&borrowed).unwrap();
        let len = data.len();
        data[len - 2..].copy_from_slice(&current.to_le_bytes());
        data
    }

    fn trigger(crank: Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            HYDRA_PROGRAM_ID,
            &[TRIGGER],
            vec![
                AccountMeta::new(crank, false),
                AccountMeta::new(Pubkey::new_unique(), true),
                AccountMeta::new_readonly(instructions_sysvar::ID, false),
            ],
        )
    }

    fn follower() -> Instruction {
        Instruction::new_with_bytes(CRANK_PROGRAM_ID, &[0; 8], vec![])
    }

    #[test]
    fn accepts_trigger_of_the_same_crank_right_before() {
        let crank = Pubkey::new_unique();
        let sysvar = sysvar_with(&[trigger(crank), follower()], 1);
        assert!(follows_trigger_of(&sysvar, &crank));
    }

    #[test]
    fn rejects_other_crank_program_or_position() {
        let crank = Pubkey::new_unique();
        // Trigger of another crank.
        let sysvar = sysvar_with(&[trigger(Pubkey::new_unique()), follower()], 1);
        assert!(!follows_trigger_of(&sysvar, &crank));
        // Not a hydra instruction.
        let mut fake = trigger(crank);
        fake.program_id = Pubkey::new_unique();
        assert!(!follows_trigger_of(
            &sysvar_with(&[fake, follower()], 1),
            &crank
        ));
        // Another hydra instruction (Cancel) of the same crank.
        let mut cancel = trigger(crank);
        cancel.data = vec![hydra_api::consts::ix::CANCEL];
        assert!(!follows_trigger_of(
            &sysvar_with(&[cancel, follower()], 1),
            &crank
        ));
        // Trigger is not directly before the current instruction.
        let sysvar = sysvar_with(&[trigger(crank), follower(), follower()], 2);
        assert!(!follows_trigger_of(&sysvar, &crank));
        // First instruction of the transaction.
        let sysvar = sysvar_with(&[follower(), trigger(crank)], 0);
        assert!(!follows_trigger_of(&sysvar, &crank));
        // Truncated sysvar.
        assert!(!follows_trigger_of(&sysvar[..sysvar.len() / 2], &crank));
        assert!(!follows_trigger_of(&[], &crank));
    }
}

/// Real Engine transactions: `ExecuteCrank` reads its preceding sibling from
/// the instructions sysvar, which the single-instruction mock cannot model.
/// Hydra is not loadable here, so a stub builtin at hydra's program id stands
/// in for `Trigger`; the byte-for-byte template check is hydra's own and is
/// covered in the hydra repository.
#[cfg(test)]
mod engine_tests {
    use engine::testkit::{Pacing, TestEngine};
    use keeper::testkit::{Dirs, keeper_builder};
    use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
    use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
    use solana_program_runtime::{
        declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
    };
    use solana_transaction::TransactionError;

    use super::*;
    use crate::{crank::execute_crank_instruction, magicblock_processor::CrankEntrypoint};

    const PROBE_ID: Pubkey = Pubkey::from_str_const("SignerProbe11111111111111111111111111111111");

    declare_process_instruction!(TriggerStub, 1, |_invoke_context| { Ok(()) });

    // Succeeds only when its first account signed.
    declare_process_instruction!(SignerProbe, 1, |invoke_context| {
        let ix_ctx = invoke_context.transaction_context.get_current_instruction_context()?;
        if ix_ctx.is_instruction_account_signer(0)? {
            Ok(())
        } else {
            Err(InstructionError::MissingRequiredSignature)
        }
    });

    fn crank_account(
        authority: [u8; 32],
        seed: [u8; 32],
        authority_signer: u8,
    ) -> AccountSharedData {
        let mut data = vec![0; CRANK_HEADER_SIZE];
        data[CRANK_AUTHORITY..CRANK_AUTHORITY + 32].copy_from_slice(&authority);
        data[CRANK_SEED..CRANK_SEED + 32].copy_from_slice(&seed);
        data[CRANK_AUTHORITY_SIGNER] = authority_signer;
        let mut account = AccountSharedData::new(1_000_000, data.len(), &HYDRA_PROGRAM_ID);
        solana_account::WritableAccount::data_as_mut_slice(&mut account).copy_from_slice(&data);
        account
    }

    struct Setup {
        engine: TestEngine,
        user: Pubkey,
        good: Pubkey,
        foreign: Pubkey,
        misbound: Pubkey,
    }

    const TASK_ID: i64 = 7;

    async fn setup() -> Setup {
        let dirs = Dirs::default();
        let mut builder = keeper_builder(&dirs);
        builder.builtins.insert(
            CRANK_PROGRAM_ID,
            (CrankEntrypoint::vm, CrankEntrypoint::codegen),
        );
        builder
            .builtins
            .insert(HYDRA_PROGRAM_ID, (TriggerStub::vm, TriggerStub::codegen));
        builder.builtins.insert(PROBE_ID, (SignerProbe::vm, SignerProbe::codegen));
        // The validator authority is only known once the engine starts, so the
        // validator-created cranks are seeded with the builder's authority.
        let validator = builder.authority.pubkey();
        let user = Pubkey::new_unique();
        let seed = task_crank_seed(&user, TASK_ID);
        let (good, foreign, misbound) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        for (key, account) in [
            (good, crank_account(validator.to_bytes(), seed, 1)),
            // Claims the validator as authority without its signature.
            (foreign, crank_account(validator.to_bytes(), seed, 0)),
            // Validator-created, but for another task.
            (
                misbound,
                crank_account(validator.to_bytes(), task_crank_seed(&user, TASK_ID + 1), 1),
            ),
        ] {
            builder.possibly_missing_accounts.insert(
                key,
                AccountBuilder::from(account).mode(AccountMode::Delegated).build(),
            );
        }
        let engine = TestEngine::from_builder(dirs, builder, Pacing::External).await;
        assert_eq!(engine.authority(), validator);
        Setup {
            engine,
            user,
            good,
            foreign,
            misbound,
        }
    }

    fn trigger(crank: Pubkey, cranker: Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            HYDRA_PROGRAM_ID,
            &[TRIGGER],
            vec![
                AccountMeta::new(crank, false),
                AccountMeta::new(cranker, true),
                AccountMeta::new_readonly(instructions_sysvar::ID, false),
            ],
        )
    }

    fn probe(signer: Pubkey) -> Instruction {
        Instruction::new_with_bytes(PROBE_ID, &[], vec![AccountMeta::new_readonly(signer, true)])
    }

    fn instruction_error(result: Result<(), TransactionError>) -> InstructionError {
        match result {
            Err(TransactionError::InstructionError(_, err)) => err,
            other => panic!("expected an instruction error, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn crank_signer_signs_only_for_validator_created_cranks_after_trigger() {
        let Setup {
            engine,
            user,
            good,
            foreign,
            misbound,
        } = setup().await;
        let cranker = engine.authority();
        let signer = crank_signer_pda(&user);
        let execute = |crank| execute_crank_instruction(crank, user, TASK_ID, vec![probe(signer)]);

        // Hydra-triggered crank of the task: the inner instruction is signed.
        engine.execute(&[trigger(good, cranker), execute(good)]).await.unwrap();

        // Without the preceding Trigger anyone could replay arbitrary payloads.
        assert_eq!(
            instruction_error(engine.execute(&[execute(good)]).await),
            InstructionError::InvalidInstructionData
        );
        // A Trigger of a different crank does not authorize this one.
        assert_eq!(
            instruction_error(engine.execute(&[trigger(foreign, cranker), execute(good)]).await),
            InstructionError::InvalidInstructionData
        );
        // Cranks the validator did not create carry untrusted templates.
        assert_eq!(
            instruction_error(engine.execute(&[trigger(foreign, cranker), execute(foreign)]).await),
            InstructionError::IncorrectAuthority
        );
        // A validator crank of another task cannot sign for this one.
        assert_eq!(
            instruction_error(
                engine.execute(&[trigger(misbound, cranker), execute(misbound)]).await
            ),
            InstructionError::InvalidSeeds
        );
        // Another authority's crank signer is never granted.
        let other = execute_crank_instruction(
            good,
            user,
            TASK_ID,
            vec![probe(crank_signer_pda(&Pubkey::new_unique()))],
        );
        assert_eq!(
            instruction_error(engine.execute(&[trigger(good, cranker), other]).await),
            InstructionError::MissingRequiredSignature
        );
    }
}
