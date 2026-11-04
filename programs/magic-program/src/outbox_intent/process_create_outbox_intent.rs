use std::collections::HashSet;

use magic_program_api::OUTBOX_INTENT_PROGRAM_ID;
use solana_account::{AccountMode, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use crate::{
    outbox_intent::outbox_intent_bundles::OutboxIntentBundle,
    utils::{
        account_actions::set_account_mode,
        accounts::{get_instruction_account_with_idx, get_instruction_pubkey_with_idx},
    },
    validator::authority,
};

const SPONSOR_IDX: u16 = 0;
const PDA_IDX: u16 = 1;

/// Creates and populates the outbox intent PDA. CPI-only, called by the
/// magic program's `AcceptScheduleCommits`. Initializes independent local state,
/// using a zero-balance target prepared locally before acceptance.
pub(crate) fn process_create_outbox_intent(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext<'_, '_>,
    data: Vec<u8>,
) -> Result<(), InstructionError> {
    validate(&signers, invoke_context, &data)?;

    let pda = get_instruction_account_with_idx(invoke_context.transaction_context, PDA_IDX)?;
    let mut acc = pda.borrow_mut()?;
    if !acc.is_any([AccountMode::Missing, AccountMode::Magic])
        || acc.lamports() != 0
        || *acc.owner() != system_program::ID
        || !acc.data().is_empty()
    {
        return Err(InstructionError::InvalidAccountData);
    }

    acc.set_owner(OUTBOX_INTENT_PROGRAM_ID);
    acc.resize(data.len(), 0);
    acc.data_as_mut_slice().copy_from_slice(&data);
    if !acc.is(AccountMode::Magic) {
        set_account_mode(invoke_context, &mut acc, AccountMode::Magic)?;
    }
    Ok(())
}

fn validate(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext<'_, '_>,
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

    let sponsor = *get_instruction_pubkey_with_idx(transaction_context, SPONSOR_IDX)?;
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

    Ok(())
}
