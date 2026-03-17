use std::collections::HashSet;

use magicblock_magic_program_api::args::MagicIntentBundleArgs;
use solana_account::state_traits::StateMut;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    magic_scheduled_base_intent::{
        CommitType, ConstructionContext, ScheduledIntentBundle,
    },
    magic_sys::fetch_current_commit_nonces,
    schedule_transactions::{
        check_magic_context_id, get_clock,
        get_parent_program_id, MAGIC_CONTEXT_IDX, PAYER_IDX,
    },
    utils::{
        account_actions::mark_account_as_undelegated,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
        charge_delegated_payer,
    },
    MagicContext,
};
use crate::schedule_transactions::{check_commit_limits, try_get_fee_vault};

pub(crate) fn process_schedule_intent_bundle(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    args: MagicIntentBundleArgs,
    secure: bool,
) -> Result<(), InstructionError> {
    check_magic_context_id(invoke_context, MAGIC_CONTEXT_IDX)?;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ScheduleAction ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    // Assert Payer is signer
    let payer_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
    if !signers.contains(payer_pubkey) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: payer pubkey {} not in signers",
            payer_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }
    let payer_account =
        get_instruction_account_with_idx(transaction_context, PAYER_IDX)?;
    let magic_fee_vault = try_get_fee_vault(
        transaction_context,
        invoke_context,
        PAYER_IDX,
        MAGIC_CONTEXT_IDX + 1,
    )?;

    //
    // Get the program_id of the parent instruction that invoked this one via CPI
    //

    // We cannot easily simulate the transaction being invoked via CPI
    // from the owning program during unit tests
    // Instead the integration tests ensure that this works as expected
    let parent_program_id =
        get_parent_program_id(transaction_context, invoke_context)?;

    let clock = get_clock(invoke_context)?;

    // NOTE: this is only protected by all the above checks however if the
    // instruction fails for other reasons detected afterward then the commit
    // stays scheduled
    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
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

    // Get next intent id
    let intent_id = context.next_intent_id();

    // Determine id and slot
    let construction_context = ConstructionContext::new(
        parent_program_id,
        &signers,
        transaction_context,
        invoke_context,
        secure,
    );

    let undelegated_accounts_ref =
        if let Some(ref value) = args.commit_and_undelegate {
            Some(CommitType::extract_commit_accounts(
                value.committed_accounts_indices(),
                construction_context.transaction_context,
            )?)
        } else {
            None
        };

    let scheduled_intent = ScheduledIntentBundle::try_new(
        args,
        intent_id,
        clock.slot,
        payer_pubkey,
        &construction_context,
    )?;

    let mut undelegated_pubkeys = vec![];
    if let Some(undelegated_accounts_ref) = undelegated_accounts_ref.as_ref() {
        // Change owner to dlp and set undelegating flag
        // Once account is undelegated we need to make it immutable in our validator.
        for (pubkey, account_ref) in undelegated_accounts_ref.iter() {
            undelegated_pubkeys.push(pubkey.to_string());
            mark_account_as_undelegated(account_ref);
        }
    }
    if !undelegated_pubkeys.is_empty() {
        ic_msg!(
            invoke_context,
            "Scheduling undelegation for accounts: {}",
            undelegated_pubkeys.join(", ")
        );
    }

    if let Some(magic_fee_vault) = magic_fee_vault {
        let chargable_accounts = scheduled_intent.get_all_committed_accounts();
        let nonces = fetch_current_commit_nonces(&chargable_accounts)?;
        let fee = scheduled_intent.calculate_fee(&nonces)?;
        charge_delegated_payer(payer_account, magic_fee_vault, fee)?;
    } else if let Some(commit_accounts) =
        scheduled_intent.get_commit_intent_accounts()
    {
        check_commit_limits(commit_accounts, invoke_context)?;
    }

    let action_sent_signature = scheduled_intent.sent_transaction.signatures[0];

    context.add_scheduled_action(scheduled_intent);
    context_data.set_state(&context)?;

    ic_msg!(invoke_context, "Scheduled commit with ID: {}", intent_id);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent signature: {}",
        action_sent_signature,
    );

    Ok(())
}
