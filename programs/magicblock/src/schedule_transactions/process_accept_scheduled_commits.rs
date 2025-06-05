use std::collections::HashSet;

use solana_log_collector::ic_msg;
use solana_program_runtime::{
    __private::{InstructionError, ReadableAccount},
    invoke_context::InvokeContext,
};

use crate::{
    schedule_transactions,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    validator::validator_authority_id,
    MagicContext, Pubkey, TransactionScheduler,
};

pub fn process_accept_scheduled_commits(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    const VALIDATOR_AUTHORITY_IDX: u16 = 0;
    const MAGIC_CONTEXT_IDX: u16 = VALIDATOR_AUTHORITY_IDX + 1;

    let transaction_context = &invoke_context.transaction_context.clone();

    // 1. Read all scheduled commits from the `MagicContext` account
    //    We do this first so we can skip all checks in case there is nothing
    //    to be processed
    schedule_transactions::check_magic_context_id(
        invoke_context,
        MAGIC_CONTEXT_IDX,
    )?;
    let magic_context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    let mut magic_context =
        bincode::deserialize::<MagicContext>(magic_context_acc.borrow().data())
            .map_err(|err| {
                ic_msg!(
                    invoke_context,
                    "Failed to deserialize MagicContext: {}",
                    err
                );
                InstructionError::InvalidAccountData
            })?;
    if magic_context.scheduled_commits.is_empty() {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits: no scheduled commits to accept"
        );
        // NOTE: we should have not been called if no commits are scheduled
        return Ok(());
    }

    // 2. Check that the validator authority (first account) is correct and signer
    let provided_validator_auth = get_instruction_pubkey_with_idx(
        transaction_context,
        VALIDATOR_AUTHORITY_IDX,
    )?;
    let validator_auth = validator_authority_id();
    if !provided_validator_auth.eq(&validator_auth) {
        ic_msg!(
             invoke_context,
             "AcceptScheduledCommits ERR: invalid validator authority {}, should be {}",
             provided_validator_auth,
             validator_auth
         );
        return Err(InstructionError::InvalidArgument);
    }
    if !signers.contains(&validator_auth) {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: validator authority pubkey {} not in signers",
            validator_auth
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // 3. Move scheduled commits (without copying)
    let scheduled_commits = magic_context.take_scheduled_commits();
    ic_msg!(
        invoke_context,
        "AcceptScheduledCommits: accepted {} scheduled commit(s)",
        scheduled_commits.len()
    );
    TransactionScheduler::default().accept_scheduled_actions(scheduled_commits);

    // 4. Serialize and store the updated `MagicContext` account
    // Zero fill account before updating data
    // NOTE: this may become expensive, but is a security measure and also prevents
    // accidentally interpreting old data when deserializing
    magic_context_acc
        .borrow_mut()
        .set_data_from_slice(&MagicContext::ZERO);

    magic_context_acc
        .borrow_mut()
        .serialize_data(&magic_context)
        .map_err(|err| {
            ic_msg!(
                invoke_context,
                "Failed to serialize MagicContext: {}",
                err
            );
            InstructionError::GenericError
        })?;

    Ok(())
}
