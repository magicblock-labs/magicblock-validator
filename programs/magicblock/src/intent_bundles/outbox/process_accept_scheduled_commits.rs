use std::collections::HashSet;

use magicblock_core::intent::outbox::outbox_intent_pda_with_bump;
use solana_account::{AccountMode, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_transaction_context::transaction::TransactionContext;

use crate::{
    MagicContext,
    intent_bundles::outbox_intent_bundles::OutboxIntentBundle,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    schedule_transactions,
    utils::{
        account_actions::set_account_mode,
        accounts::{
            InstructionAccount, get_instruction_account_with_idx,
            get_instruction_pubkey_with_idx,
        },
    },
    validator::authority,
};

const VALIDATOR_AUTHORITY_IDX: u16 = 0;
const MAGIC_PROGRAM_ID: u16 = VALIDATOR_AUTHORITY_IDX + 1;
const MAGIC_CONTEXT_IDX: u16 = MAGIC_PROGRAM_ID + 1;
const INTENT_PDAS_OFFSET: u16 = MAGIC_CONTEXT_IDX + 1;

pub fn process_accept_scheduled_commits(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    // Common conditions verification
    let validator_auth = authority();
    validate(&signers, invoke_context, &validator_auth)?;

    // pop first n intents
    // n - is number of OutboxIntentBundle PDAs passed
    let intents = pop_scheduled_intents(invoke_context)?;
    if intents.is_empty() {
        // NOTE: we should have not been called if no commits are scheduled
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: no scheduled commits to accept"
        );
        return Ok(());
    }

    for (i, intent) in intents.into_iter().enumerate() {
        let pda_idx = INTENT_PDAS_OFFSET + i as u16;
        let bump = verify_intent_pda(invoke_context, intent.id, pda_idx)?;

        // Create outbox ephemeral account
        create_outbox_ephemeral_account(
            invoke_context,
            pda_idx,
            OutboxIntentBundle::accepted(intent, bump),
        )?;
    }

    Ok(())
}

fn validate(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    validator_auth: &Pubkey,
) -> Result<(), InstructionError> {
    // Check magic context
    schedule_transactions::check_magic_context_id(
        invoke_context,
        MAGIC_CONTEXT_IDX,
    )?;

    let transaction_context = &*invoke_context.transaction_context;

    // Assert magic program account
    let magic_program_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, MAGIC_PROGRAM_ID)?;
    if *magic_program_pubkey != crate::id() {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: account at idx {} is {}, expected magic program {}",
            MAGIC_PROGRAM_ID,
            magic_program_pubkey,
            crate::id()
        );
        return Err(InstructionError::IncorrectProgramId);
    }

    // Assert validator authority
    let provided_validator_auth = get_instruction_pubkey_with_idx(
        transaction_context,
        VALIDATOR_AUTHORITY_IDX,
    )?;
    if provided_validator_auth != validator_auth {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: invalid validator authority {}, should be {}",
            provided_validator_auth,
            validator_auth
        );
        return Err(InstructionError::InvalidArgument);
    }

    // Validate authority is a signer
    if !signers.contains(validator_auth) {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: validator authority pubkey {} not in signers",
            validator_auth
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    Ok(())
}

fn verify_intent_pda(
    invoke_context: &InvokeContext,
    intent_id: u64,
    pda_idx: u16,
) -> Result<u8, InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;
    let provided =
        get_instruction_pubkey_with_idx(transaction_context, pda_idx)?;
    let (expected, bump) = outbox_intent_pda_with_bump(intent_id);
    if *provided != expected {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: account at idx {} is {}, expected PDA {} for intent {}",
            pda_idx,
            provided,
            expected,
            intent_id
        );
        return Err(InstructionError::InvalidArgument);
    }
    Ok(bump)
}

fn pop_scheduled_intents(
    invoke_context: &InvokeContext,
) -> Result<Vec<ScheduledIntentBundle>, InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;
    let num_ix_accounts = transaction_context
        .get_current_instruction_context()?
        .get_number_of_instruction_accounts()
        as usize;

    // Assert enough accounts
    let num_accept_intents = match num_ix_accounts
        .checked_sub(INTENT_PDAS_OFFSET as usize)
    {
        Some(0) => {
            // No outbox intent PDAs provided - nothing to accept
            return Ok(vec![]);
        }
        Some(count) => count,
        None => {
            ic_msg!(
                invoke_context,
                "AcceptScheduledCommits ERR: not enough accounts to accept intents ({}), need validator authority, magic program, magic context, and at least one outbox intent PDA",
                num_ix_accounts
            );
            return Err(InstructionError::MissingAccount);
        }
    };

    let magic_context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    let mut magic_context = MagicContext::deserialize(
        magic_context_acc.borrow()?.data(),
    )
    .map_err(|err| {
        ic_msg!(
            invoke_context,
            "Failed to deserialize MagicContext: {}",
            err
        );
        InstructionError::InvalidAccountData
    })?;

    let intents =
        magic_context.take_front_scheduled_commits(num_accept_intents);
    if intents.len() != num_accept_intents {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: requested {} intents but only {} available",
            num_accept_intents,
            intents.len()
        );

        return Err(InstructionError::InvalidArgument);
    }

    // Write updated account data
    magic_context
        .write_to(magic_context_acc.borrow_mut()?.data_as_mut_slice())?;

    Ok(intents)
}

fn create_outbox_ephemeral_account(
    invoke_context: &InvokeContext,
    pda_idx: u16,
    outbox_account: OutboxIntentBundle,
) -> Result<(), InstructionError> {
    let intent_id = outbox_account.inner.id;
    let data = outbox_account.try_to_bytes().map_err(|_| {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: failed to serialize intent {}",
            intent_id
        );
        InstructionError::InvalidAccountData
    })?;

    let transaction_context = &*invoke_context.transaction_context;
    let ephemeral = validate_new_ephemeral(transaction_context, pda_idx)?;

    let mut acc = ephemeral.borrow_mut()?;
    acc.set_owner(crate::id());
    acc.resize(data.len(), 0);
    acc.data_as_mut_slice().copy_from_slice(&data);
    set_account_mode(invoke_context, &mut acc, AccountMode::Ephemeral)?;

    Ok(())
}

/// Validates that the account at [`EPHEMERAL_IDX`] is an empty system-owned
/// account (0 lamports, system program owner). Returns the account for
/// initialization.
fn validate_new_ephemeral<'a, 'ix_data>(
    tc: &'a TransactionContext<'ix_data>,
    idx: u16,
) -> Result<InstructionAccount<'a, 'ix_data>, InstructionError> {
    let ephemeral = get_instruction_account_with_idx(tc, idx)?;
    let acc = ephemeral.borrow()?;
    if acc.lamports() != 0 || *acc.owner() != system_program::ID {
        return Err(InstructionError::InvalidAccountData);
    }
    drop(acc);
    Ok(ephemeral)
}
