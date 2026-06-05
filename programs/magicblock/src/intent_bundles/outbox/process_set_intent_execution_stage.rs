use std::collections::HashSet;

use magicblock_core::intent::outbox::outbox_intent_pda;
use magicblock_magic_program_api::outbox::ExecutionStage;
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    intent_bundles::outbox_intent_bundles::OutboxIntentBundle,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    validator::effective_validator_authority_id,
};

const VALIDATOR_AUTHORITY_IDX: u16 = 0;
const INTENT_PDA_IDX: u16 = 1;

pub fn process_set_intent_execution_stage(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    intent_id: u64,
    stage: ExecutionStage,
) -> Result<(), InstructionError> {
    let validator_auth = effective_validator_authority_id();
    verify(&signers, invoke_context, &validator_auth, intent_id)?;

    let transaction_context = &*invoke_context.transaction_context;
    let intent_acc =
        get_instruction_account_with_idx(transaction_context, INTENT_PDA_IDX)?;

    let mut bundle =
        OutboxIntentBundle::try_from_bytes(intent_acc.borrow()?.data())
            .map_err(|_| {
                ic_msg!(
                    invoke_context,
                    "SetIntentExecutionStage ERR: failed to deserialize outbox intent {}",
                    intent_id
                );
                InstructionError::InvalidAccountData
            })?;

    bundle.apply_stage_transition(stage).map_err(|reason| {
        ic_msg!(
            invoke_context,
            "SetIntentExecutionStage ERR: intent {}: invalid transition: {}",
            intent_id,
            reason
        );
        InstructionError::InvalidArgument
    })?;

    let data = bundle.try_to_bytes().map_err(|_| {
        ic_msg!(
            invoke_context,
            "SetIntentExecutionStage ERR: failed to serialize outbox intent {}",
            intent_id
        );
        InstructionError::InvalidAccountData
    })?;
    intent_acc
        .borrow_mut()?
        .data_as_mut_slice()
        .copy_from_slice(&data);

    Ok(())
}

fn verify(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    validator_auth: &Pubkey,
    intent_id: u64,
) -> Result<(), InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;

    let provided_validator_auth = get_instruction_pubkey_with_idx(
        transaction_context,
        VALIDATOR_AUTHORITY_IDX,
    )?;
    if provided_validator_auth != validator_auth {
        ic_msg!(
            invoke_context,
            "SetIntentExecutionStage ERR: invalid validator authority {}, should be {}",
            provided_validator_auth,
            validator_auth
        );
        return Err(InstructionError::InvalidArgument);
    }
    if !signers.contains(validator_auth) {
        ic_msg!(
            invoke_context,
            "SetIntentExecutionStage ERR: validator authority {} not in signers",
            validator_auth
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    let provided_pda =
        get_instruction_pubkey_with_idx(transaction_context, INTENT_PDA_IDX)?;
    let expected_pda = outbox_intent_pda(intent_id);
    if *provided_pda != expected_pda {
        ic_msg!(
            invoke_context,
            "SetIntentExecutionStage ERR: account at idx {} is {}, expected PDA {} for intent {}",
            INTENT_PDA_IDX,
            provided_pda,
            expected_pda,
            intent_id
        );
        return Err(InstructionError::InvalidArgument);
    }

    Ok(())
}
