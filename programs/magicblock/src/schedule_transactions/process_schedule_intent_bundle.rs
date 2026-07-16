use std::collections::HashSet;

use magicblock_magic_program_api::args::MagicIntentBundleArgs;
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    magic_scheduled_base_intent::{
        calculate_rent_pending_ata_materialization_fee,
        extract_commit_accounts, ConstructionContext, ScheduledIntentBundle,
    },
    schedule_transactions::{
        check_commit_limits, check_magic_context_id,
        fetch_commit_nonces_with_missing_as_zero, get_clock,
        get_parent_program_id, try_get_fee_vault, MAGIC_CONTEXT_IDX, PAYER_IDX,
    },
    utils::{
        account_actions::{
            charge_delegated_payer, mark_account_as_undelegated,
        },
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
    },
    MagicContext,
};

pub(crate) fn process_schedule_intent_bundle(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    args: MagicIntentBundleArgs,
    secure: bool,
) -> Result<(), InstructionError> {
    check_magic_context_id(invoke_context, MAGIC_CONTEXT_IDX)?;

    let parent_program_id = get_parent_program_id(invoke_context)?;
    let clock = get_clock(invoke_context)?;

    let (payer_pubkey, mut context) = {
        let transaction_context = &*invoke_context.transaction_context;
        let ix_ctx = transaction_context.get_current_instruction_context()?;

        if ix_ctx.get_program_key()? != &crate::id() {
            ic_msg!(
                invoke_context,
                "ScheduleAction ERR: Magic program account not found"
            );
            return Err(InstructionError::UnsupportedProgramId);
        }

        let payer_pubkey =
            *get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
        if !signers.contains(&payer_pubkey) {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: payer pubkey {} not in signers",
                payer_pubkey
            );
            return Err(InstructionError::MissingRequiredSignature);
        }

        let context_acc = get_instruction_account_with_idx(
            transaction_context,
            MAGIC_CONTEXT_IDX,
        )?;
        let context = MagicContext::deserialize(context_acc.borrow()?.data())
            .map_err(|err| {
            ic_msg!(
                invoke_context,
                "Failed to deserialize MagicContext: {}",
                err
            );
            InstructionError::GenericError
        })?;

        (payer_pubkey, context)
    };

    // Get next intent id
    let intent_id = context.next_intent_id();

    let rent_pending_materialization_accounts = {
        let transaction_context = &*invoke_context.transaction_context;
        let fee_vault = try_get_fee_vault(
            transaction_context,
            invoke_context,
            PAYER_IDX,
            MAGIC_CONTEXT_IDX + 1,
        )?;
        if fee_vault.is_some() {
            Some((
                payer_pubkey,
                *get_instruction_pubkey_with_idx(
                    transaction_context,
                    MAGIC_CONTEXT_IDX + 1,
                )?,
            ))
        } else {
            None
        }
    };

    // Determine id and slot
    let (undelegated_pubkeys, scheduled_intent) = {
        let mut construction_context = ConstructionContext::new(
            parent_program_id,
            &signers,
            invoke_context,
            secure,
            rent_pending_materialization_accounts,
        );

        // Collect all undelegated account indices.
        let undelegated_account_indices: Vec<u8> = [
            args.commit_and_undelegate.as_ref(),
            args.commit_finalize_and_undelegate.as_ref(),
        ]
        .into_iter()
        .flatten()
        .flat_map(|el| el.committed_accounts_indices())
        .copied()
        .collect();

        // Recreate intent
        let scheduled_intent = ScheduledIntentBundle::try_new(
            args,
            intent_id,
            clock.slot,
            &payer_pubkey,
            &mut construction_context,
        )?;

        if rent_pending_materialization_accounts.is_some() {
            let transaction_context =
                construction_context.transaction_context();
            let payer_account = get_instruction_account_with_idx(
                transaction_context,
                PAYER_IDX,
            )?;
            let magic_fee_vault = get_instruction_account_with_idx(
                transaction_context,
                MAGIC_CONTEXT_IDX + 1,
            )?;
            let rent_pending_pubkeys = scheduled_intent
                .intent_bundle
                .rent_pending_ata_materializations
                .iter()
                .map(|materialization| materialization.eata_pubkey)
                .collect::<HashSet<_>>();
            let committed_accounts =
                scheduled_intent.get_all_committed_accounts();
            let nonces = fetch_commit_nonces_with_missing_as_zero(
                &committed_accounts,
                &rent_pending_pubkeys,
            )?;
            let fee = scheduled_intent
                .calculate_fee(&nonces)?
                .checked_add(calculate_rent_pending_ata_materialization_fee(
                    scheduled_intent
                        .intent_bundle
                        .rent_pending_ata_materializations
                        .len(),
                )?)
                .ok_or(InstructionError::ArithmeticOverflow)?;
            // Charge before local undelegation clears a self-payer's delegated bit.
            charge_delegated_payer(&payer_account, &magic_fee_vault, fee)?;
        } else if let Some(commit_accounts) =
            scheduled_intent.get_commit_intent_accounts()
        {
            check_commit_limits(
                commit_accounts,
                construction_context.invoke_context,
            )?;
        }

        // Change owner to dlp and set undelegating flag.
        // Once account is undelegated we need to make it immutable in our validator.
        let transaction_context = construction_context.transaction_context();
        let undelegated_accounts_ref = extract_commit_accounts(
            &undelegated_account_indices,
            transaction_context,
        )?;
        let mut undelegated_pubkeys =
            Vec::with_capacity(undelegated_accounts_ref.len());
        for (pubkey, account_ref) in undelegated_accounts_ref.iter() {
            undelegated_pubkeys.push(pubkey.to_string());
            mark_account_as_undelegated(account_ref)?;
        }

        (undelegated_pubkeys, scheduled_intent)
    };
    if !undelegated_pubkeys.is_empty() {
        ic_msg!(
            invoke_context,
            "Scheduling undelegation for accounts: {}",
            undelegated_pubkeys.join(", ")
        );
    }

    let transaction_context = &*invoke_context.transaction_context;
    let action_sent_signature = scheduled_intent.sent_transaction.signatures[0];

    context.add_scheduled_action(scheduled_intent);
    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    context.write_to(context_acc.borrow_mut()?.data_as_mut_slice())?;

    ic_msg!(invoke_context, "Scheduled commit with ID: {}", intent_id);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent signature: {}",
        action_sent_signature,
    );

    Ok(())
}
