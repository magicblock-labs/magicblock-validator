use std::collections::HashSet;

use magicblock_core::intent::BaseActionCallback;
use magicblock_magic_program_api::args::AddActionCallbackArgs;
use solana_account::state_traits::StateMut;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    schedule_transactions::{
        check_magic_context_id, get_clock, get_parent_program_id,
        try_get_fee_vault, MAGIC_CONTEXT_IDX, PAYER_IDX,
    },
    utils::{
        account_actions::charge_delegated_payer,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
    },
    MagicContext,
};

const CALLBACK_FEE_LAMPORTS: u64 = 5_000;

pub(crate) fn process_add_action_callback(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    args: AddActionCallbackArgs,
) -> Result<(), InstructionError> {
    // This function requires vault to be present
    const MAGIC_FEE_VAULT_IDX: u16 = MAGIC_CONTEXT_IDX + 1;

    check_magic_context_id(invoke_context, MAGIC_CONTEXT_IDX)?;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "Schedule ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    let payer_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
    let payer_acc =
        get_instruction_account_with_idx(transaction_context, PAYER_IDX)?;
    if !signers.contains(payer_pubkey) {
        ic_msg!(
            invoke_context,
            "AddActionCallback ERR: payer {} not in signers",
            payer_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    let magic_fee_vault = try_get_fee_vault(
        transaction_context,
        invoke_context,
        PAYER_IDX,
        MAGIC_FEE_VAULT_IDX
    )?.ok_or(InstructionError::MissingAccount)
    .inspect_err(|_| {
        ic_msg!(
            invoke_context,
            "AddActionCallback ERR: magic fee vault account required to be passed"
        );
    })?;

    // Charge User for callback
    charge_delegated_payer(payer_acc, magic_fee_vault, CALLBACK_FEE_LAMPORTS)?;

    let context_data = &mut context_acc.borrow_mut();
    let mut context =
        MagicContext::deserialize(context_data).map_err(|err| {
            ic_msg!(
                invoke_context,
                "Failed to deserialize MagicContext: {}",
                err
            );
            InstructionError::GenericError
        })?;

    let latest_intent =
        context.scheduled_base_intents.last_mut().ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "AddActionCallback ERR: no scheduled intents found"
            );
            InstructionError::InvalidAccountData
        })?;

    let action = latest_intent
        .intent_bundle
        .get_action_mut(args.action_index as usize)
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "AddActionCallback ERR: action index {} out of range",
                args.action_index
            );
            InstructionError::InvalidInstructionData
        })?;

    // Validate if the caller has right to set callback
    if action.callback.is_some() {
        ic_msg!(
            invoke_context,
            "AddActionCallback ERR: callback already set for action at index {}",
            args.action_index
        );
        return Err(InstructionError::InvalidAccountData);
    }
    let Some(source_program) = action.source_program else {
        ic_msg!(
            invoke_context,
            "AddActionCallback ERR: callbacks requires intent scheduled via ScheduleIntentBundle"
        );
        return Err(InstructionError::InvalidAccountData);
    };
    let clock = get_clock(invoke_context)?;
    let parent_program_id =
        get_parent_program_id(transaction_context, invoke_context)?;
    if Some(source_program) != parent_program_id {
        ic_msg!(
            invoke_context,
            "AddActionCallback ERR: CPI caller {:?} does not match action source_program {}",
            parent_program_id,
            source_program
        );
        return Err(InstructionError::InvalidAccountData);
    }
    if latest_intent.slot != clock.slot
        || latest_intent.blockhash
            != invoke_context.environment_config.blockhash
    {
        ic_msg!(
            invoke_context,
            "AddActionCallback ERR: intent was scheduled in a different slot or blockhash"
        );
        return Err(InstructionError::InvalidInstructionData);
    }

    action.callback = Some(BaseActionCallback {
        destination_program: args.destination_program,
        discriminator: args.discriminator,
        payload: args.payload,
        compute_units: args.compute_units,
        account_metas_per_program: args.accounts,
    });

    context_data.set_state(&context)?;

    ic_msg!(
        invoke_context,
        "Attached callback to action at index {}",
        args.action_index
    );

    Ok(())
}
