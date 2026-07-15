use std::collections::HashSet;

use magicblock_core::intent::outbox::outbox_intent_pda;
use magicblock_magic_program_api::{
    instruction::OutboxIntentInstruction, EPHEMERAL_SYSTEM_PROGRAM_ID,
    EPHEMERAL_VAULT_PUBKEY, OUTBOX_INTENT_PROGRAM_ID,
};
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent::outbox_intent_bundles::OutboxIntentBundle,
    schedule_transactions,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    validator::effective_validator_authority_id,
    MagicContext,
};

const VALIDATOR_AUTHORITY_IDX: u16 = 0;
const OUTBOX_PROGRAM_IDX: u16 = VALIDATOR_AUTHORITY_IDX + 1;
const MAGIC_CONTEXT_IDX: u16 = OUTBOX_PROGRAM_IDX + 1;
const VAULT_IDX: u16 = MAGIC_CONTEXT_IDX + 1;
const EPHEMERAL_SYSTEM_PROGRAM_IDX: u16 = VAULT_IDX + 1;
const INTENT_PDAS_OFFSET: u16 = EPHEMERAL_SYSTEM_PROGRAM_IDX + 1;

pub fn process_accept_scheduled_commits(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    // Common conditions verification
    let validator_auth = effective_validator_authority_id();
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
        let pda = verify_intent_pda(invoke_context, intent.id, pda_idx)?;

        // Create outbox ephemeral account
        create_outbox_intent_cpi(
            invoke_context,
            validator_auth,
            pda,
            OutboxIntentBundle::accepted(intent),
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

    // Assert outbox intent program account (CPI target)
    let outbox_program_pubkey = get_instruction_pubkey_with_idx(
        transaction_context,
        OUTBOX_PROGRAM_IDX,
    )?;
    if *outbox_program_pubkey != OUTBOX_INTENT_PROGRAM_ID {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: account at idx {} is {}, expected outbox intent program {}",
            OUTBOX_PROGRAM_IDX,
            outbox_program_pubkey,
            OUTBOX_INTENT_PROGRAM_ID
        );
        return Err(InstructionError::IncorrectProgramId);
    }

    // Assert ephemeral system program account (CPI target, two levels deep)
    let ephemeral_system_program_pubkey = get_instruction_pubkey_with_idx(
        transaction_context,
        EPHEMERAL_SYSTEM_PROGRAM_IDX,
    )?;
    if *ephemeral_system_program_pubkey != EPHEMERAL_SYSTEM_PROGRAM_ID {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: account at idx {} is {}, expected ephemeral system program {}",
            EPHEMERAL_SYSTEM_PROGRAM_IDX,
            ephemeral_system_program_pubkey,
            EPHEMERAL_SYSTEM_PROGRAM_ID
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
) -> Result<Pubkey, InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;
    let provided =
        get_instruction_pubkey_with_idx(transaction_context, pda_idx)?;
    let expected = outbox_intent_pda(intent_id);
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
    Ok(expected)
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
                "AcceptScheduledCommits ERR: not enough accounts to accept intents ({}), need validator authority, magic context, vault, and at least one outbox intent PDA",
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

fn create_outbox_intent_cpi(
    invoke_context: &mut InvokeContext,
    validator_auth: Pubkey,
    pda: Pubkey,
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

    invoke_context.native_invoke(
        Instruction {
            program_id: OUTBOX_INTENT_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(validator_auth, true),
                AccountMeta::new(pda, false),
                AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
                AccountMeta::new_readonly(EPHEMERAL_SYSTEM_PROGRAM_ID, false),
            ],
            data: OutboxIntentInstruction::CreateOutboxIntent { data }
                .try_to_vec()
                .map_err(|_| InstructionError::InvalidInstructionData)?,
        },
        &[],
    )
}
