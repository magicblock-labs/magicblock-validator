// The crank signer is deprecated for programs but still executed here.
#![allow(deprecated)]

use std::mem::offset_of;

use hydra_api::{
    CRANK_HEADER_SIZE, consts::ix::TRIGGER, ephemeral, instruction::scheduled_ixs_from_crank,
    state::Crank,
};
use magic_program_api::CRANK_PROGRAM_ID;
use solana_account::ReadableAccount;
use solana_instruction::{Instruction, TRANSACTION_LEVEL_STACK_HEIGHT, error::InstructionError};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::instructions as instructions_sysvar;

use crate::{
    schedule_task::validate_cranks_instructions,
    utils::accounts::{get_instruction_account_with_idx, get_instruction_pubkey_with_idx},
    validator::authority as validator_authority,
};

/// Index of the hydra crank in `ExecuteCrank`'s accounts.
const EXECUTE_CRANK_CRANK_IDX: u16 = 0;
/// Index of the instructions sysvar in `ExecuteCrank`'s accounts.
const EXECUTE_CRANK_SYSVAR_IDX: u16 = 1;
const HYDRA_PROGRAM_ID: Pubkey = Pubkey::new_from_array(ephemeral::ID.to_bytes());
const CRANK_AUTHORITY: usize = offset_of!(Crank, authority);
const CRANK_AUTHORITY_SIGNER: usize = offset_of!(Crank, authority_signer);
const SERIALIZED_META_SIZE: usize = 33;

/// Runs the instructions stored in a hydra crank with the crank signer that
/// they mark as a signer.
///
/// Anyone may submit the hydra `Trigger` that precedes this instruction, so
/// authorization comes from the crank rather than from the transaction signer:
/// 1. `Trigger` of crank `C` sits directly before this instruction, so hydra
///    charged and advanced `C` for this execution.
/// 2. `C` is hydra-owned and was created and signed for by the validator
///    authority, so its instructions were written by the task scheduler, which
///    only stores tasks that `ScheduleTask` accepted. `ScheduleTask` allows a
///    single signer: the scheduling authority's own crank signer, read-only.
/// 3. The instructions run are read from `C` itself, never from this
///    instruction, and are checked again as `ScheduleTask` checked them.
pub(crate) fn process_execute_crank(
    invoke_context: &mut InvokeContext<'_, '_>,
) -> Result<(), InstructionError> {
    let instructions = stored_instructions(invoke_context)?;
    let crank_signer = crank_signer_of(invoke_context, &instructions)?;
    validate_cranks_instructions(invoke_context, &crank_signer, &instructions)?;

    let len = instructions.len();
    for ix in instructions {
        invoke_context.native_invoke_as(CRANK_PROGRAM_ID, ix, &[crank_signer])?;
    }
    ic_msg!(invoke_context, "Executed crank with {} instructions", len);
    Ok(())
}

/// Validates the crank and returns the instructions stored in it.
fn stored_instructions(
    invoke_context: &InvokeContext<'_, '_>,
) -> Result<Vec<Instruction>, InstructionError> {
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
        ic_msg!(invoke_context, "ExecuteCrank ERR: missing crank or sysvar");
        return Err(InstructionError::MissingAccount);
    }

    let crank = *get_instruction_pubkey_with_idx(tc, EXECUTE_CRANK_CRANK_IDX)?;
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
    scheduled_ixs_from_crank(data).ok_or_else(|| {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: crank {} holds malformed instructions",
            crank
        );
        InstructionError::InvalidAccountData
    })
}

/// The one crank signer that `instructions` mark as a signer.
///
/// `ScheduleTask` only stores the scheduling authority's own crank signer as
/// a signer, so a crank with no signer, or with two, was not stored by it. A
/// crank without a signer is replayed top-level and never comes here.
fn crank_signer_of(
    invoke_context: &InvokeContext<'_, '_>,
    instructions: &[Instruction],
) -> Result<Pubkey, InstructionError> {
    let mut signers = instructions
        .iter()
        .flat_map(|ix| &ix.accounts)
        .filter(|meta| meta.is_signer)
        .map(|meta| meta.pubkey);
    let Some(signer) = signers.next() else {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: crank instructions need no crank signer"
        );
        return Err(InstructionError::InvalidAccountData);
    };
    if signers.any(|other| other != signer) {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: crank instructions need more than one signer"
        );
        return Err(InstructionError::MissingRequiredSignature);
    }
    Ok(signer)
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
    fn crank_signer_pda_is_unchanged() {
        // Same derivation the Crank program used before cranks moved to hydra.
        let authority = Pubkey::new_unique();
        let (expected, _) = Pubkey::find_program_address(
            &[b"crank-executor", authority.as_ref()],
            &Pubkey::from_str_const("Crank11111111111111111111111111111111111111"),
        );
        assert_eq!(
            magic_program_api::pda::crank_signer_pda(&authority),
            expected
        );
    }

    #[test]
    fn hydra_knows_the_crank_program_and_execute_crank() {
        // Hydra's `Trigger` matches the delegated follow-up against these.
        assert_eq!(
            hydra_api::consts::crank_program::ID.to_bytes(),
            CRANK_PROGRAM_ID.to_bytes()
        );
        let data =
            wincode::serialize(&magic_program_api::instruction::CrankInstruction::ExecuteCrank)
                .unwrap();
        assert_eq!(data, hydra_api::consts::CRANK_EXECUTE_DATA);
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
/// The stored task is a v42 expression that fails unless the authority's
/// crank signer actually signed, then a v42 transfer that counts executions.
/// Hydra is not loadable here, so a stub builtin at hydra's program id stands
/// in for `Trigger`; hydra's own check of the follow-up is covered in the
/// hydra repository.
#[cfg(test)]
mod engine_tests {
    use engine::testkit::{Pacing, TestEngine};
    use hydra_api::{consts::META_FLAG_SIGNER, consts::META_FLAG_WRITABLE};
    use keeper::testkit::{Dirs, V42_ID, keeper_builder};
    use magic_program_api::pda::crank_signer_pda;
    use solana_account::{AccountBuilder, AccountMode, AccountSharedData, ReadableAccount};
    use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
    use solana_program_runtime::{
        declare_process_instruction, solana_sbpf::program::BuiltinFunctionDefinition,
    };
    use solana_transaction::TransactionError;
    use v42_calculator_interface::builder::{Expr as E, transfer};

    use super::*;
    use crate::magicblock_processor::CrankEntrypoint;

    declare_process_instruction!(TriggerStub, 1, |_invoke_context| { Ok(()) });

    /// A hydra crank holding `instructions` as hydra stores them.
    fn crank_account(
        authority: Pubkey,
        authority_signer: u8,
        instructions: &[Instruction],
    ) -> AccountSharedData {
        let mut data = vec![0; CRANK_HEADER_SIZE];
        data[CRANK_AUTHORITY..CRANK_AUTHORITY + 32].copy_from_slice(authority.as_ref());
        data[CRANK_AUTHORITY_SIGNER] = authority_signer;
        for ix in instructions {
            data.extend_from_slice(&(ix.accounts.len() as u16).to_le_bytes());
            for meta in &ix.accounts {
                let mut flag = 0;
                if meta.is_signer {
                    flag |= META_FLAG_SIGNER;
                }
                if meta.is_writable {
                    flag |= META_FLAG_WRITABLE;
                }
                data.push(flag);
                data.extend_from_slice(meta.pubkey.as_ref());
            }
            data.extend_from_slice(ix.program_id.as_ref());
            data.extend_from_slice(&(ix.data.len() as u16).to_le_bytes());
            data.extend_from_slice(&ix.data);
        }
        let mut account = AccountSharedData::new(1_000_000, data.len(), &HYDRA_PROGRAM_ID);
        solana_account::WritableAccount::data_as_mut_slice(&mut account).copy_from_slice(&data);
        account
    }

    /// v42 account the counting transfers draw from.
    const SOURCE: Pubkey = Pubkey::from_str_const("Source1111111111111111111111111111111111111");

    /// Task instructions: a v42 expression that fails unless the crank signer
    /// of `authority` signed, then a v42 transfer that bumps `counter` by one.
    /// Under CPI v42 returns its result instead of writing it, so the transfer
    /// does the counting; it lands only if the check before it passed.
    fn task(authority: &Pubkey, counter: Pubkey) -> Vec<Instruction> {
        let signer = crank_signer_pda(authority);
        let check = E::lit(1).require_signer(1).compose_signed(counter, &[signer], signer);
        vec![check, transfer(SOURCE, counter, 1)]
    }

    struct Crank {
        pubkey: Pubkey,
        instructions: Vec<Instruction>,
    }

    struct Setup {
        engine: TestEngine,
        counter: Pubkey,
        cranks: Vec<Crank>,
    }

    /// Starts an engine holding one crank per `(authority, authority_signer,
    /// instructions)`, where `None` as authority stands for the validator.
    async fn setup(counter: Pubkey, cranks: Vec<(Option<Pubkey>, u8, Vec<Instruction>)>) -> Setup {
        let dirs = Dirs::default();
        let mut builder = keeper_builder(&dirs);
        builder.builtins.insert(
            CRANK_PROGRAM_ID,
            (CrankEntrypoint::vm, CrankEntrypoint::codegen),
        );
        builder
            .builtins
            .insert(HYDRA_PROGRAM_ID, (TriggerStub::vm, TriggerStub::codegen));
        // The validator authority is only known once the engine starts, so the
        // validator-created cranks are seeded with the builder's authority.
        let validator = builder.authority.pubkey();
        let mut accounts = vec![
            (counter, AccountSharedData::new(1_000_000, 8, &V42_ID)),
            (SOURCE, AccountSharedData::new(1_000_000, 8, &V42_ID)),
        ];
        let cranks = cranks
            .into_iter()
            .map(|(authority, authority_signer, instructions)| {
                let pubkey = Pubkey::new_unique();
                let authority = authority.unwrap_or(validator);
                accounts.push((
                    pubkey,
                    crank_account(authority, authority_signer, &instructions),
                ));
                Crank { pubkey, instructions }
            })
            .collect();
        for (key, account) in accounts {
            builder.possibly_missing_accounts.insert(
                key,
                AccountBuilder::from(account).mode(AccountMode::Delegated).build(),
            );
        }
        let engine = TestEngine::from_builder(dirs, builder, Pacing::External).await;
        assert_eq!(engine.authority(), validator);
        Setup { engine, counter, cranks }
    }

    fn trigger(crank: Pubkey, cranker: Pubkey, nonce: u8) -> Instruction {
        // The stub ignores the nonce, which keeps repeated transactions distinct.
        Instruction::new_with_bytes(
            HYDRA_PROGRAM_ID,
            &[TRIGGER, nonce],
            vec![
                AccountMeta::new(crank, false),
                AccountMeta::new(cranker, true),
                AccountMeta::new_readonly(instructions_sysvar::ID, false),
            ],
        )
    }

    /// What the hydra cranker sends after `Trigger` of `crank`.
    fn execute(crank: &Crank) -> Instruction {
        let execute =
            hydra_api::instruction::ephemeral::execute_crank(crank.pubkey, &crank.instructions);
        // Hydra's own builder must produce this program's instruction.
        assert_eq!(execute.program_id, CRANK_PROGRAM_ID);
        execute
    }

    fn count(engine: &TestEngine, counter: Pubkey) -> u64 {
        let account = engine.get_account(counter).unwrap();
        u64::from_le_bytes(account.data()[..8].try_into().unwrap())
    }

    fn instruction_error(result: Result<(), TransactionError>) -> InstructionError {
        match result {
            Err(TransactionError::InstructionError(_, err)) => err,
            other => panic!("expected an instruction error, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn crank_signer_signs_stored_tasks_of_validator_cranks_after_trigger() {
        let user = Pubkey::new_unique();
        let counter = Pubkey::new_unique();
        let Setup { engine, counter, cranks } = setup(
            counter,
            vec![
                (None, 1, task(&user, counter)),
                // Claims the validator as authority without its signature.
                (None, 0, task(&user, counter)),
                // Created by someone else.
                (Some(Pubkey::new_unique()), 1, task(&user, counter)),
            ],
        )
        .await;
        let [good, unsigned, foreign] = &cranks[..] else { unreachable!() };
        let cranker = engine.authority();

        // Hydra-triggered crank: the v42 check sees the crank signer as a
        // signer, and the stored instructions run in order.
        for nonce in 0..2 {
            engine
                .execute(&[trigger(good.pubkey, cranker, nonce), execute(good)])
                .await
                .unwrap();
        }
        assert_eq!(count(&engine, counter), 2);

        // Without the preceding Trigger the crank would run for free.
        assert_eq!(
            instruction_error(engine.execute(&[execute(good)]).await),
            InstructionError::InvalidInstructionData
        );
        // A Trigger of a different crank does not authorize this one.
        assert_eq!(
            instruction_error(
                engine.execute(&[trigger(foreign.pubkey, cranker, 0), execute(good)]).await
            ),
            InstructionError::InvalidInstructionData
        );
        // Cranks the validator did not create carry untrusted instructions.
        for crank in [unsigned, foreign] {
            assert_eq!(
                instruction_error(
                    engine.execute(&[trigger(crank.pubkey, cranker, 0), execute(crank)]).await
                ),
                InstructionError::IncorrectAuthority
            );
        }
        // Only the two authorized executions ran.
        assert_eq!(count(&engine, counter), 2);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn execute_crank_rejects_instructions_schedule_task_would_reject() {
        let user = Pubkey::new_unique();
        let counter = Pubkey::new_unique();
        let signer = crank_signer_pda(&user);
        let mut writable_signer = task(&user, counter);
        writable_signer[0].accounts[1].is_writable = true;
        let mut two_signers = task(&user, counter);
        two_signers[1].accounts.push(AccountMeta::new_readonly(
            crank_signer_pda(&Pubkey::new_unique()),
            true,
        ));
        let mut nested = task(&user, counter);
        nested[1].program_id = CRANK_PROGRAM_ID;
        let no_signer = vec![transfer(SOURCE, counter, 1)];
        assert_eq!(writable_signer[0].accounts[1].pubkey, signer);
        let Setup { engine, counter, cranks } = setup(
            counter,
            vec![
                (None, 1, writable_signer),
                (None, 1, two_signers),
                (None, 1, nested),
                (None, 1, no_signer),
            ],
        )
        .await;
        let cranker = engine.authority();
        let expected = [
            InstructionError::Immutable,
            InstructionError::MissingRequiredSignature,
            InstructionError::IncorrectProgramId,
            // Cranks without a signer are replayed top-level instead.
            InstructionError::InvalidAccountData,
        ];
        for (crank, expected) in cranks.iter().zip(expected) {
            assert_eq!(
                instruction_error(
                    engine.execute(&[trigger(crank.pubkey, cranker, 0), execute(crank)]).await
                ),
                expected
            );
        }
        assert_eq!(count(&engine, counter), 0);
    }
}
