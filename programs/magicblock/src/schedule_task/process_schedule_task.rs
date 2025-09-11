use std::collections::HashSet;

use magicblock_magic_program_api::args::ScheduleTaskArgs;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey};
#[cfg(test)]
use solana_sdk::{signature::Keypair, signer::Signer};

#[cfg(not(test))]
use crate::validator::validator_authority_id;
use crate::{
    schedule_task::utils::check_task_context_id,
    task_context::{ScheduleTaskRequest, TaskContext, MIN_EXECUTION_INTERVAL},
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    TaskRequest,
};

#[cfg(test)]
lazy_static::lazy_static! {
    static ref VALIDATOR_AUTHORITY_ID: Pubkey = Keypair::new().pubkey();
}

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

    // Assert that the task instructions do not have signers aside from the validator authority
    for instruction in &args.instructions {
        for account in &instruction.accounts {
            #[cfg(not(test))]
            let val_id = validator_authority_id();
            #[cfg(test)]
            let val_id = *VALIDATOR_AUTHORITY_ID;
            if account.is_signer && account.pubkey.ne(&val_id) {
                return Err(InstructionError::MissingRequiredSignature);
            }
        }
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
    TaskContext::add_request(
        context_acc,
        TaskRequest::Schedule(schedule_request),
    )?;

    ic_msg!(
        invoke_context,
        "Scheduled task request with ID: {}",
        args.task_id
    );

    Ok(())
}

#[cfg(test)]
mod test {
    use magicblock_magic_program_api::{
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

    fn create_simple_ix() -> Instruction {
        Instruction::new_with_borsh(MEMO_PROGRAM_ID, b"test memo", vec![])
    }

    fn create_complex_ix(
        pdas: &[Pubkey],
        writable: bool,
        signer: bool,
    ) -> Instruction {
        Instruction::new_with_borsh(
            COUNTER_PROGRAM_ID,
            b"",
            pdas.iter()
                .map(|pda| {
                    if writable {
                        AccountMeta::new(*pda, signer)
                    } else {
                        AccountMeta::new_readonly(*pda, signer)
                    }
                })
                .collect(),
        )
    }

    fn setup_accounts(
        n_pdas: usize,
    ) -> (Keypair, Vec<Pubkey>, Vec<(Pubkey, AccountSharedData)>) {
        let payer = Keypair::new();
        let pdas = (0..n_pdas)
            .map(|_| Keypair::new().pubkey())
            .collect::<Vec<_>>();
        let mut transaction_accounts = vec![
            (
                payer.pubkey(),
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
            (
                TASK_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, 0, &system_program::id()),
            ),
        ];
        transaction_accounts.extend(
            pdas.iter()
                .map(|pda| {
                    (
                        *pda,
                        AccountSharedData::new(
                            u64::MAX,
                            0,
                            &Keypair::new().pubkey(),
                        ),
                    )
                })
                .collect::<Vec<_>>(),
        );
        (payer, pdas, transaction_accounts)
    }

    fn setup_simple_ix_test() -> (Vec<(Pubkey, AccountSharedData)>, Instruction)
    {
        let (payer, _pdas, transaction_accounts) = setup_accounts(0);

        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 10,
            iterations: 1,
            instructions: vec![create_simple_ix()],
        };
        let ix =
            InstructionUtils::schedule_task_instruction(&payer.pubkey(), args);

        (transaction_accounts, ix)
    }

    fn setup_complex_ix_test(
        n_pdas: usize,
        writable: bool,
        signer: bool,
    ) -> (Vec<(Pubkey, AccountSharedData)>, Instruction) {
        let (payer, pdas, transaction_accounts) = setup_accounts(n_pdas);

        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 10,
            iterations: 1,
            instructions: vec![create_complex_ix(&pdas, writable, signer)],
        };
        let ix =
            InstructionUtils::schedule_task_instruction(&payer.pubkey(), args);

        (transaction_accounts, ix)
    }

    #[test]
    fn test_process_schedule_task_simple() {
        let (transaction_accounts, ix) = setup_simple_ix_test();
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    fn test_process_schedule_complex_task() {
        let (tx_accs, ix) = setup_complex_ix_test(2, false, false);
        process_instruction(&ix.data, tx_accs, ix.accounts, Ok(()));

        let (tx_accs, ix) = setup_complex_ix_test(2, true, false);
        process_instruction(&ix.data, tx_accs, ix.accounts, Ok(()));
    }

    #[test]
    fn fail_process_schedule_task_with_instructions_signers() {
        // Read only signer
        let (tx_accs, ix) = setup_complex_ix_test(2, false, true);
        process_instruction(
            &ix.data,
            tx_accs,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );

        // Writable signer
        let (tx_accs, ix) = setup_complex_ix_test(2, true, true);
        process_instruction(
            &ix.data,
            tx_accs,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }

    #[test]
    fn fail_unsigned_process_schedule_task() {
        let (payer, _pdas, transaction_accounts) = setup_accounts(0);
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 1000,
            iterations: 1,
            instructions: vec![create_simple_ix()],
        };
        let account_metas = vec![
            AccountMeta::new(payer.pubkey(), false),
            AccountMeta::new(TASK_CONTEXT_PUBKEY, false),
        ];
        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleTask(args),
            account_metas,
        );
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
        let (payer, _pdas, transaction_accounts) = setup_accounts(0);
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 1000,
            iterations: 1,
            instructions: vec![],
        };
        let ix =
            InstructionUtils::schedule_task_instruction(&payer.pubkey(), args);
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidInstructionData),
        );
    }

    #[test]
    fn fail_process_schedule_invalid_execution_interval() {
        let (payer, _pdas, transaction_accounts) = setup_accounts(0);
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 9,
            iterations: 1,
            instructions: vec![create_simple_ix()],
        };
        let ix =
            InstructionUtils::schedule_task_instruction(&payer.pubkey(), args);
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidInstructionData),
        );
    }
}
