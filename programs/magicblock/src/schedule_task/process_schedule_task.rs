use std::collections::HashSet;

use magicblock_magic_program_api::args::ScheduleTaskArgs;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey};

use crate::{
    schedule_task::utils::{check_accounts_signers, check_task_context_id},
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    Task, TaskContext,
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
    if ix_accs_len <= ACCOUNTS_START {
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

    // Check that all writable accounts in the task instructions are present in the instruction pubkeys
    let instruction_pubkeys = (ACCOUNTS_START..ix_accs_len)
        .map(|idx| {
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)
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
        if !instruction_pubkeys.contains(&writable_pubkey) {
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

    let task = Task::new(
        args.task_id,
        args.instructions,
        *payer_pubkey,
        args.period_millis,
        args.n_executions,
    );

    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        TASK_CONTEXT_IDX,
    )?;
    let context_data = &mut context_acc.borrow_mut();
    let mut context =
        TaskContext::deserialize(context_data).map_err(|err| {
            ic_msg!(
                invoke_context,
                "Failed to deserialize MagicContext: {}",
                err
            );
            InstructionError::GenericError
        })?;
    context.add_task(task);
    context_data.serialize_data(&context).map_err(|err| {
        ic_msg!(invoke_context, "Failed to serialize TaskContext: {}", err);
        InstructionError::GenericError
    })?;
    ic_msg!(invoke_context, "Scheduled task with ID: {}", args.task_id);

    Ok(())
}
