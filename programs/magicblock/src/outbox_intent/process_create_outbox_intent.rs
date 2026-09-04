use std::collections::HashSet;

use magicblock_magic_program_api::OUTBOX_INTENT_PROGRAM_ID;
use solana_account::{AccountMode, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_transaction_context::transaction::TransactionContext;

use crate::{
    outbox_intent::outbox_intent_bundles::OutboxIntentBundle,
    utils::{
        account_actions::set_account_mode,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
    },
    validator::authority,
};

const SPONSOR_IDX: u16 = 0;
const PDA_IDX: u16 = 1;

/// Creates and populates the outbox intent PDA. CPI-only, called by the
/// magic program's `AcceptScheduleCommits`. Claims ownership of the fresh,
/// system-owned, zero-lamport PDA directly - no ephemeral system program CPI
/// is needed since no lamports are transferred.
pub fn process_create_outbox_intent(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    data: Vec<u8>,
) -> Result<(), InstructionError> {
    validate(&signers, invoke_context, &data)?;
    create_ephemeral_outbox_account(invoke_context, PDA_IDX, data)
}

fn validate(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    data: &[u8],
) -> Result<(), InstructionError> {
    OutboxIntentBundle::try_from_bytes(data).map_err(|_| {
        ic_msg!(
            invoke_context,
            "CreateOutboxIntent ERR: data is not a valid OutboxIntentBundle"
        );
        InstructionError::InvalidInstructionData
    })?;

    let transaction_context = &*invoke_context.transaction_context;

    let sponsor =
        *get_instruction_pubkey_with_idx(transaction_context, SPONSOR_IDX)?;
    let validator_auth = authority();
    if sponsor != validator_auth {
        ic_msg!(
            invoke_context,
            "CreateOutboxIntent ERR: invalid sponsor {}, should be validator authority {}",
            sponsor,
            validator_auth
        );
        return Err(InstructionError::IncorrectAuthority);
    }
    if !signers.contains(&validator_auth) {
        ic_msg!(
            invoke_context,
            "CreateOutboxIntent ERR: validator authority {} not in signers",
            validator_auth
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    validate_new_pda(transaction_context, PDA_IDX)
}

/// Validates that the account at `idx` is an empty system-owned account
/// (0 lamports, system program owner), ready to be claimed as the outbox PDA.
fn validate_new_pda(
    transaction_context: &TransactionContext,
    idx: u16,
) -> Result<(), InstructionError> {
    let pda_acc = get_instruction_account_with_idx(transaction_context, idx)?;
    let acc = pda_acc.borrow()?;
    if acc.lamports() != 0 || *acc.owner() != system_program::ID {
        return Err(InstructionError::InvalidAccountData);
    }
    Ok(())
}

fn create_ephemeral_outbox_account(
    invoke_context: &InvokeContext,
    pda_idx: u16,
    data: Vec<u8>,
) -> Result<(), InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;
    let pda_acc =
        get_instruction_account_with_idx(transaction_context, pda_idx)?;

    let mut acc = pda_acc.borrow_mut()?;
    acc.set_owner(OUTBOX_INTENT_PROGRAM_ID);
    acc.resize(data.len(), 0);
    acc.data_as_mut_slice().copy_from_slice(&data);
    set_account_mode(invoke_context, &mut acc, AccountMode::Ephemeral)
}
