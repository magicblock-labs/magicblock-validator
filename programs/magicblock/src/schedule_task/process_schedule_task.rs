use std::collections::HashSet;

use magicblock_core::tls::ExecutionTlsStash;
use magicblock_magic_program_api::args::{
    ScheduleTaskArgs, ScheduleTaskRequest, TaskRequest,
};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    schedule_task::validate_cranks_instructions,
    utils::accounts::get_instruction_pubkey_with_idx,
};

pub(crate) fn process_schedule_task(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    args: ScheduleTaskArgs,
) -> Result<(), InstructionError> {
    const PAYER_IDX: u16 = 0;

    const ACCOUNTS_START: usize = PAYER_IDX as usize + 1;

    let payer_pubkey = {
        let transaction_context = &*invoke_context.transaction_context;
        let ix_ctx = transaction_context.get_current_instruction_context()?;
        let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;

        // Assert MagicBlock program
        if ix_ctx.get_program_key()? != &crate::id() {
            ic_msg!(
                invoke_context,
                "ScheduleTask ERR: program ID mismatch, expected {}",
                crate::id()
            );
            return Err(InstructionError::UnsupportedProgramId);
        }

        // Assert enough accounts
        if ix_accs_len < ACCOUNTS_START {
            ic_msg!(
                invoke_context,
                "ScheduleTask ERR: not enough accounts to schedule task ({}), need payer, signing program and task context",
                ix_accs_len
            );
            return Err(InstructionError::MissingAccount);
        }

        // Assert Payer is signer
        let payer_pubkey =
            *get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
        if !signers.contains(&payer_pubkey) {
            ic_msg!(
                invoke_context,
                "ScheduleTask ERR: payer pubkey {} not in signers",
                payer_pubkey
            );
            return Err(InstructionError::MissingRequiredSignature);
        }

        payer_pubkey
    };

    // Enforce minimal number of iterations
    if args.iterations < 1 {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: iterations must be at least 1"
        );
        return Err(InstructionError::InvalidInstructionData);
    }

    // Enforce valid interval
    if args.execution_interval_millis <= 0
        || args.execution_interval_millis > u32::MAX as i64
    {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: execution interval must be between 1 and {} milliseconds",
            u32::MAX
        );
        return Err(InstructionError::InvalidInstructionData);
    }

    // Enforce minimal number of instructions
    if args.instructions.is_empty() {
        ic_msg!(
            invoke_context,
            "ScheduleTask ERR: instructions must be non-empty"
        );
        return Err(InstructionError::InvalidInstructionData);
    }

    validate_cranks_instructions(invoke_context, &args.instructions)?;

    let schedule_request = ScheduleTaskRequest {
        id: args.task_id,
        instructions: args.instructions,
        authority: payer_pubkey,
        execution_interval_millis: args.execution_interval_millis,
        iterations: args.iterations,
    };

    // Add schedule request to execution TLS stash
    ExecutionTlsStash::register_task(TaskRequest::Schedule(schedule_request));

    ic_msg!(
        invoke_context,
        "Scheduled task request with ID: {}",
        args.task_id
    );

    Ok(())
}

#[cfg(test)]
mod test {
    use magicblock_magic_program_api::instruction::MagicBlockInstruction;
    use solana_account::AccountSharedData;
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_sdk_ids::system_program;
    use solana_signer::Signer;

    use super::*;
    use crate::{
        test_utils::{process_instruction, COUNTER_PROGRAM_ID},
        utils::instruction_utils::InstructionUtils,
        validator::{
            generate_validator_authority_if_needed, validator_authority_id,
        },
    };

    fn create_simple_ix() -> Instruction {
        InstructionUtils::noop_instruction(0)
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
        generate_validator_authority_if_needed();
        let payer = Keypair::new();
        let pdas = (0..n_pdas)
            .map(|_| Keypair::new().pubkey())
            .collect::<Vec<_>>();
        let transaction_accounts = vec![(
            payer.pubkey(),
            AccountSharedData::new(u64::MAX, 0, &system_program::id()),
        )];
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
    fn fail_process_schedule_task_with_validator_authority() {
        let (payer, mut pdas, transaction_accounts) = setup_accounts(0);
        pdas.push(validator_authority_id());
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 1000,
            iterations: 1,
            instructions: vec![create_complex_ix(&pdas, false, false)],
        };
        let ix =
            InstructionUtils::schedule_task_instruction(&payer.pubkey(), args);
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectAuthority),
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
        let account_metas = vec![AccountMeta::new(payer.pubkey(), false)];
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
    fn test_process_schedule_task_with_invalid_iterations() {
        let (payer, _pdas, transaction_accounts) = setup_accounts(0);
        let args = ScheduleTaskArgs {
            task_id: 1,
            execution_interval_millis: 1000,
            iterations: -100,
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

    #[test]
    fn test_process_schedule_task_with_invalid_execution_interval() {
        let (payer, _pdas, transaction_accounts) = setup_accounts(0);
        for execution_interval_millis in [-12345, 0, u32::MAX as i64 + 1] {
            let args = ScheduleTaskArgs {
                task_id: 1,
                execution_interval_millis,
                iterations: 1,
                instructions: vec![create_simple_ix()],
            };
            let ix = InstructionUtils::schedule_task_instruction(
                &payer.pubkey(),
                args,
            );
            process_instruction(
                &ix.data,
                transaction_accounts.clone(),
                ix.accounts,
                Err(InstructionError::InvalidInstructionData),
            );
        }
    }
}
