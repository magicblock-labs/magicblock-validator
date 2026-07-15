use std::collections::HashSet;

use magicblock_magic_program_api::{
    instruction::EphemeralSystemInstruction, EPHEMERAL_SYSTEM_PROGRAM_ID,
};
use solana_account::WritableAccount;
use solana_instruction::{error::InstructionError, AccountMeta, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    validator::effective_validator_authority_id,
};

const SPONSOR_IDX: u16 = 0;
const PDA_IDX: u16 = 1;
const VAULT_IDX: u16 = 2;
const EPHEMERAL_SYSTEM_PROGRAM_IDX: u16 = 3;

/// Creates and populates the outbox intent PDA. CPI-only, called by the
/// magic program's `AcceptScheduleCommits`. CPIs into the ephemeral system
/// program to create the account (which becomes its owner, since we are the
/// immediate caller), then writes `data` into it directly.
pub fn process_create_outbox_intent(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    data: Vec<u8>,
) -> Result<(), InstructionError> {
    let (sponsor, pda, vault) = validate(&signers, invoke_context)?;

    create_ephemeral_account_cpi(
        invoke_context,
        sponsor,
        pda,
        vault,
        data.len() as u32,
    )?;

    let transaction_context = &*invoke_context.transaction_context;
    let pda_acc =
        get_instruction_account_with_idx(transaction_context, PDA_IDX)?;
    pda_acc
        .borrow_mut()?
        .data_as_mut_slice()
        .copy_from_slice(&data);

    Ok(())
}

fn validate(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
) -> Result<(Pubkey, Pubkey, Pubkey), InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;

    let sponsor =
        *get_instruction_pubkey_with_idx(transaction_context, SPONSOR_IDX)?;
    let validator_auth = effective_validator_authority_id();
    if sponsor != validator_auth {
        ic_msg!(
            invoke_context,
            "CreateOutboxIntent ERR: invalid sponsor {}, should be validator authority {}",
            sponsor,
            validator_auth
        );
        return Err(InstructionError::InvalidArgument);
    }
    if !signers.contains(&validator_auth) {
        ic_msg!(
            invoke_context,
            "CreateOutboxIntent ERR: validator authority {} not in signers",
            validator_auth
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    let ephemeral_system_program = get_instruction_pubkey_with_idx(
        transaction_context,
        EPHEMERAL_SYSTEM_PROGRAM_IDX,
    )?;
    if *ephemeral_system_program != EPHEMERAL_SYSTEM_PROGRAM_ID {
        ic_msg!(
            invoke_context,
            "CreateOutboxIntent ERR: account at idx {} is {}, expected ephemeral system program {}",
            EPHEMERAL_SYSTEM_PROGRAM_IDX,
            ephemeral_system_program,
            EPHEMERAL_SYSTEM_PROGRAM_ID
        );
        return Err(InstructionError::IncorrectProgramId);
    }

    let pda = *get_instruction_pubkey_with_idx(transaction_context, PDA_IDX)?;
    let vault =
        *get_instruction_pubkey_with_idx(transaction_context, VAULT_IDX)?;

    Ok((sponsor, pda, vault))
}

fn create_ephemeral_account_cpi(
    invoke_context: &mut InvokeContext,
    sponsor: Pubkey,
    pda: Pubkey,
    vault: Pubkey,
    data_len: u32,
) -> Result<(), InstructionError> {
    invoke_context.native_invoke(
        Instruction {
            program_id: EPHEMERAL_SYSTEM_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(sponsor, true),
                AccountMeta::new(pda, true),
                AccountMeta::new(vault, false),
            ],
            data: EphemeralSystemInstruction::CreateEphemeralAccount {
                data_len,
            }
            .try_to_vec()
            .map_err(|_| InstructionError::InvalidInstructionData)?,
        },
        &[pda],
    )
}
