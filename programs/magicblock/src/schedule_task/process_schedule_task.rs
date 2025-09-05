use std::collections::HashSet;

use magicblock_magic_program_api::args::ScheduleTaskArgs;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey};

use crate::{
    schedule_task::utils::{check_accounts_signers, check_task_context_id},
    task_context::{ScheduleTaskRequest, TaskContext, MIN_EXECUTION_INTERVAL},
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        get_writable_with_idx,
    },
};

pub(crate) fn process_schedule_task(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    args: ScheduleTaskArgs,
) -> Result<(), InstructionError> {
    const PAYER_IDX: u16 = 0;
    const TASK_CONTEXT_IDX: u16 = PAYER_IDX + 1;

    check_task_context_id(invoke_context, TASK_CONTEXT_IDX)?;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;
    const ACCOUNTS_START: usize = TASK_CONTEXT_IDX as usize + 1;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ScheduleTask ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    // Assert enough accounts
    if ix_accs_len < ACCOUNTS_START {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: not enough accounts to schedule task ({}), need payer, signing program and task context",
            ix_accs_len
        );
        return Err(InstructionError::NotEnoughAccountKeys);
    }

    // Assert Payer is signer
    let payer_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
    if !signers.contains(payer_pubkey) {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: payer pubkey {} not in signers",
            payer_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Check that all writable accounts in the task instructions are writable in the instruction pubkeys
    let instruction_pubkeys = (ACCOUNTS_START..ix_accs_len)
        .map(|idx| {
            let pubkey = match get_instruction_pubkey_with_idx(
                transaction_context,
                idx as u16,
            ) {
                Ok(pubkey) => pubkey,
                Err(e) => {
                    ic_msg!(
                        invoke_context,
                        "ScheduleTask ERR: failed to get instruction context: {}",
                        e
                    );
                    return Err(e);
                }
            };
            let writable = match get_writable_with_idx(transaction_context, idx as u16) {
                Ok(writable) => writable,
                Err(e) => {
                    ic_msg!(
                        invoke_context,
                        "ScheduleTask ERR: failed to get writable: {}",
                        e
                    );
                    return Err(e);
                }
            };
            Ok((
                pubkey,
                writable,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let writable_accounts = args
        .instructions
        .iter()
        .flat_map(|ix| {
            ix.accounts
                .iter()
                .filter_map(|acc| acc.is_writable.then_some(acc.pubkey))
        })
        .collect::<Vec<_>>();

    for writable_pubkey in &writable_accounts {
        if !instruction_pubkeys.contains(&(writable_pubkey, true)) {
            ic_msg!(
                invoke_context,
                "ScheduleTask ERR: writable account {} not provided in instruction pubkeys",
                writable_pubkey
            );
            return Err(InstructionError::InvalidAccountOwner);
        }
    }

    // Assert all provided accounts are signers or owned by the invoking program
    check_accounts_signers(
        invoke_context,
        transaction_context,
        ix_accs_len,
        ACCOUNTS_START,
        signers,
    )?;

    // Enforce minimal execution interval
    if args.execution_interval_millis < MIN_EXECUTION_INTERVAL {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: execution interval must be at least {} milliseconds",
            MIN_EXECUTION_INTERVAL
        );
        return Err(InstructionError::InvalidInstructionData);
    }

    // Enforce minimal number of executions
    if args.instructions.is_empty() {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: instructions must be non-empty"
        );
        return Err(InstructionError::InvalidInstructionData);
    }

    let schedule_request = ScheduleTaskRequest {
        id: args.task_id,
        instructions: args.instructions,
        authority: *payer_pubkey,
        execution_interval_millis: args.execution_interval_millis,
        iterations: args.iterations,
    };

    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        TASK_CONTEXT_IDX,
    )?;

    TaskContext::schedule_task(invoke_context, context_acc, schedule_request)
        .map_err(|err| {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: failed to schedule task: {}",
            err
        );
        InstructionError::GenericError
    })?;

    ic_msg!(
        invoke_context,
        "Scheduled task request with ID: {}",
        args.task_id
    );

    Ok(())
}

#[cfg(test)]
mod test {
    use magicblock_core::magic_program::{
        instruction::MagicBlockInstruction, TASK_CONTEXT_PUBKEY,
    };
    use solana_sdk::{
        account::AccountSharedData,
        instruction::{AccountMeta, Instruction},
        signature::Keypair,
        signer::Signer,
        system_program,
    };

    use super::*;
    use crate::{
        test_utils::{
            process_instruction, COUNTER_PROGRAM_ID, MEMO_PROGRAM_ID,
        },
        utils::instruction_utils::InstructionUtils,
    };

    pub fn create_simple_ix(payer: &Keypair) -> Instruction {
        Instruction::new_with_borsh(
            MEMO_PROGRAM_ID,
            b"test memo",
            vec![AccountMeta::new_readonly(payer.pubkey(), true)],
        )
    }

    pub fn create_complex_ix(
        payer: &Keypair,
        pdas: &[Pubkey],
        writable: bool,
        signer: bool,
    ) -> Instruction {
        Instruction::new_with_borsh(
            COUNTER_PROGRAM_ID,
            b"",
            vec![AccountMeta::new_readonly(payer.pubkey(), true)]
                .into_iter()
                .chain(pdas.iter().map(|pda| {
                    if writable {
                        AccountMeta::new(*pda, signer)
                    } else {
                        AccountMeta::new_readonly(*pda, signer)
                    }
                }))
                .collect(),
        )
    }

    pub fn schedule_task_instruction_test(
        payer: &Pubkey,
        args: ScheduleTaskArgs,
        pdas: &[Pubkey],
        writable: bool,
        signer: bool,
    ) -> Instruction {
        let mut account_metas = vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(TASK_CONTEXT_PUBKEY, false),
        ];
        for pubkey in pdas {
            if writable {
                account_metas.push(AccountMeta::new(*pubkey, signer));
            } else {
                account_metas.push(AccountMeta::new_readonly(*pubkey, signer));
            }
        }

        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleTask(args),
            account_metas,
        )
    }

    #[test]
    fn test_process_schedule_task_simple() {
        let pdas = vec![];
        let payer = Keypair::new();
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 10,
            iterations: 1,
            instructions: vec![create_simple_ix(&payer)],
        };
        let ix = InstructionUtils::schedule_task_instruction(
            &payer.pubkey(),
            args,
            &pdas,
        );
        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
        ];
        let expected_result = Ok(());
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }

    #[test]
    fn test_process_schedule_complex_task() {
        let pda1 = Keypair::new();
        let pda2 = Keypair::new();
        let pdas = vec![pda1.pubkey(), pda2.pubkey()];
        let payer = Keypair::new();

        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                pda1.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &COUNTER_PROGRAM_ID),
            ),
            (
                pda2.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &COUNTER_PROGRAM_ID),
            ),
        ];
        let expected_result = Ok(());

        // Writable accounts must be signers or owned by the invoking program (the owner of the first PDA)
        // This is always the case in this test
        for (writable, signer) in
            [(true, true), (false, true), (true, false), (false, false)]
        {
            let args = ScheduleTaskArgs {
                task_id: 1,
                execution_interval_millis: 10,
                iterations: 1,
                instructions: vec![create_complex_ix(
                    &payer, &pdas, writable, signer,
                )],
            };
            let ix = InstructionUtils::schedule_task_instruction(
                &payer.pubkey(),
                args,
                &pdas,
            );
            process_instruction(
                &ix.data,
                transaction_accounts.clone(),
                ix.accounts,
                expected_result.clone(),
            );
        }
    }

    #[test]
    fn fail_process_schedule_task_with_foreign_accounts() {
        let pda1 = Keypair::new();
        let pda2 = Keypair::new();
        let pdas = vec![pda1.pubkey(), pda2.pubkey()];
        let payer = Keypair::new();

        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                pda1.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &COUNTER_PROGRAM_ID),
            ),
            (
                pda2.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &MEMO_PROGRAM_ID), // Wrong owner
            ),
        ];

        for (writable, signer, expected_result) in [
            (true, true, Ok(())),
            (false, true, Err(InstructionError::InvalidAccountOwner)),
            (true, false, Err(InstructionError::InvalidAccountOwner)),
            (false, false, Err(InstructionError::InvalidAccountOwner)),
        ] {
            let args = ScheduleTaskArgs {
                task_id: 1,
                execution_interval_millis: 10,
                iterations: 1,
                instructions: vec![create_complex_ix(
                    &payer, &pdas, true, false,
                )],
            };
            let ix = schedule_task_instruction_test(
                &payer.pubkey(),
                args,
                &pdas,
                writable,
                signer,
            );
            process_instruction(
                &ix.data,
                transaction_accounts.clone(),
                ix.accounts,
                expected_result,
            );
        }
    }

    #[test]
    fn fail_unsigned_process_schedule_task() {
        let pda1 = Keypair::new().pubkey();
        let pda2 = Keypair::new().pubkey();
        let pdas = vec![pda1, pda2];
        let payer = Keypair::new();
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 1000,
            iterations: 1,
            instructions: vec![],
        };

        let mut account_metas = vec![
            AccountMeta::new(payer.pubkey(), false),
            AccountMeta::new(TASK_CONTEXT_PUBKEY, false),
        ];
        for pubkey in pdas {
            account_metas.push(AccountMeta::new_readonly(pubkey, true));
        }

        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleTask(args),
            account_metas,
        );

        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                pda1,
                AccountSharedData::new(u64::MAX, 0, &COUNTER_PROGRAM_ID),
            ),
            (
                pda2,
                AccountSharedData::new(u64::MAX, 0, &COUNTER_PROGRAM_ID),
            ),
        ];
        let expected_result = Err(InstructionError::MissingRequiredSignature);
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }

    #[test]
    fn fail_process_schedule_empty_task() {
        let pdas = vec![];
        let payer = Keypair::new();
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 1000,
            iterations: 1,
            instructions: vec![],
        };
        let ix = InstructionUtils::schedule_task_instruction(
            &payer.pubkey(),
            args,
            &pdas,
        );
        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
        ];
        let expected_result = Err(InstructionError::InvalidInstructionData);
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }

    #[test]
    fn fail_process_schedule_invalid_execution_interval() {
        let pdas = vec![];
        let payer = Keypair::new();
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 9,
            iterations: 1,
            instructions: vec![create_simple_ix(&payer)],
        };
        let ix = InstructionUtils::schedule_task_instruction(
            &payer.pubkey(),
            args,
            &pdas,
        );
        let transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
        ];
        let expected_result = Err(InstructionError::InvalidInstructionData);
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            expected_result,
        );
    }
}
