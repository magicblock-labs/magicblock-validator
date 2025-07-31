use std::collections::HashSet;

use magicblock_magic_program_api::TASK_CONTEXT_PUBKEY;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{
    account::ReadableAccount, instruction::InstructionError, pubkey::Pubkey,
    transaction_context::TransactionContext,
};

use crate::utils::accounts::{
    get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
};

pub(crate) fn check_task_context_id(
    invoke_context: &InvokeContext,
    idx: u16,
) -> Result<(), InstructionError> {
    let provided_magic_context = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        idx,
    )?;
    if !provided_magic_context.eq(&TASK_CONTEXT_PUBKEY) {
        ic_msg!(
            invoke_context,
            "ERR: invalid task context account {}",
            provided_magic_context
        );
        return Err(InstructionError::MissingAccount);
    }

    Ok(())
}

pub(crate) fn check_accounts_signers(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    ix_accs_len: usize,
    accounts_start: usize,
    signers: HashSet<Pubkey>,
) -> Result<Pubkey, InstructionError> {
    //
    // Get the program_id of the parent instruction that invoked this one via CPI
    //

    // We cannot easily simulate the transaction being invoked via CPI
    // from the owning program during unit tests
    // Instead the integration tests ensure that this works as expected
    #[cfg(not(test))]
    let frames = crate::utils::instruction_context_frames::InstructionContextFrames::try_from(transaction_context)?;

    // During unit tests we assume the first committee has the correct program ID
    #[cfg(test)]
    let first_account_owner = {
        *get_instruction_account_with_idx(
            transaction_context,
            accounts_start as u16,
        )?
        .borrow()
        .owner()
    };

    #[cfg(not(test))]
    let parent_program_id = {
        let parent_program_id =
            frames.find_program_id_of_parent_of_current_instruction();

        ic_msg!(
            invoke_context,
            "Task: parent program id: {}",
            parent_program_id
                .map_or_else(|| "None".to_string(), |id| id.to_string())
        );

        parent_program_id
    };

    #[cfg(test)]
    let parent_program_id = Some(&first_account_owner);

    let Some(parent_program_id) = parent_program_id else {
        ic_msg!(invoke_context, "Task ERR: failed to find parent program id");
        return Err(InstructionError::InvalidInstructionData);
    };

    // Assert all accounts are owned by invoking program OR are signers
    // NOTE: we don't require PDAs to be signers as in our case verifying that the
    // program owning the PDAs invoked us via CPI is sufficient
    // Thus we can be `invoke`d unsigned and no seeds need to be provided
    for idx in accounts_start..ix_accs_len {
        let acc_pubkey =
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc =
            get_instruction_account_with_idx(transaction_context, idx as u16)?;

        {
            let acc_owner = *acc.borrow().owner();
            if parent_program_id.ne(&acc_owner) && !signers.contains(acc_pubkey)
            {
                ic_msg!(
                    invoke_context,
                        "ScheduleTask ERR: account {} needs to be owned by the invoking program {} or be a signer to be used in task, but is owned by {}",
                        acc_pubkey, parent_program_id, acc_owner
                );
                return Err(InstructionError::InvalidAccountOwner);
            }
        }
    }

    Ok(*parent_program_id)
}
