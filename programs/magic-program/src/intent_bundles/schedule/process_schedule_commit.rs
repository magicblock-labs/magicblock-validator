use std::collections::HashSet;

use mbv_core::intent::{
    CommitAndUndelegate, CommitType, MagicBaseIntent, UndelegateType, calculate_commit_fee_for_ids,
    outbox::outbox_intent_pda, types::CommittedAccount,
};
use mbv_core::token_programs::try_remap_ata_to_eata;
use solana_account::{AccountMode, ReadableAccount, WritableAccount};
use solana_account_info::MAX_PERMITTED_DATA_INCREASE;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    MagicContext,
    magic_scheduled_base_intent::{ScheduledIntentBundle, validate_commit_schedule_permissions},
    schedule_transactions::{self, check_commit_limits, get_parent_program_id, try_get_fee_vault},
    schedule_transactions::{reserve_commit_id, validate_intent_size},
    utils::{
        account_actions::{charge_delegated_payer, mark_account_as_undelegated},
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
            get_writable_with_idx,
        },
    },
};

#[derive(Default)]
pub(crate) struct ProcessScheduleCommitOptions {
    pub request_undelegation: bool,
}

pub(crate) fn process_schedule_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext<'_, '_>,
    opts: ProcessScheduleCommitOptions,
) -> Result<(), InstructionError> {
    const PAYER_IDX: u16 = 0;
    const MAGIC_CONTEXT_IDX: u16 = PAYER_IDX + 1;

    schedule_transactions::check_magic_context_id(invoke_context, MAGIC_CONTEXT_IDX)?;

    let transaction_context = &*invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;

    // Assert MagicBlock program
    if ix_ctx.get_program_key()? != &crate::id() {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: Magic program account not found"
        );
        return Err(InstructionError::UnsupportedProgramId);
    }

    // Assert Payer is signer
    let payer_pubkey = get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
    if !signers.contains(payer_pubkey) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: payer pubkey {} not in signers",
            payer_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    let payer_account = get_instruction_account_with_idx(transaction_context, PAYER_IDX)?;
    let (magic_fee_vault, committees_start) = try_get_fee_vault(
        transaction_context,
        invoke_context,
        PAYER_IDX,
        MAGIC_CONTEXT_IDX + 1,
    )?;

    // Assert enough accounts
    if ix_accs_len <= committees_start {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: not enough accounts to schedule commit ({}), need payer, signing program an account for each pubkey to be committed",
            ix_accs_len
        );
        return Err(InstructionError::MissingAccount);
    }

    let parent_program_id = get_parent_program_id(invoke_context)?;

    // Assert all accounts are delegated, owned by invoking program OR are signers
    // Also works if the validator authority is a signer
    // NOTE: we don't require PDAs to be signers as in our case verifying that the
    // program owning the PDAs invoked us directly via CPI is sufficient
    // Thus we can be `invoke`d unsigned and no seeds need to be provided
    let mut accounts_to_commit = Vec::new();
    let mut seen_committed_pubkeys: HashSet<Pubkey> = HashSet::new();
    for idx in committees_start..ix_accs_len {
        let acc_pubkey = get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc = get_instruction_account_with_idx(transaction_context, idx as u16)?;

        // Local-only accounts, including Magic ATAs, cannot be committed.
        if acc.borrow()?.is(AccountMode::Magic) {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: account {} is local-only and cannot be committed to base chain",
                acc_pubkey
            );
            return Err(InstructionError::InvalidAccountData);
        }

        // Accounts larger than 10_240 bytes cannot currently be committed.
        if acc.borrow()?.data().len() > MAX_PERMITTED_DATA_INCREASE {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: account {} is too large to be committed",
                acc_pubkey
            );
            return Err(InstructionError::InvalidAccountData);
        }

        {
            let is_delegated = acc.borrow()?.is(AccountMode::Delegated);

            if opts.request_undelegation {
                // Must be writable and delegated to avoid double-undelegation
                let is_writable = get_writable_with_idx(transaction_context, idx as u16)?;
                if !is_writable || !is_delegated {
                    ic_msg!(
                        invoke_context,
                        "ScheduleCommit ERR: account {} is required to be writable and delegated in order to be undelegated",
                        acc_pubkey
                    );
                    return Err(InstructionError::ReadonlyDataModified);
                }
            } else if !is_delegated {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommit ERR: account {} is required to be delegated to the current validator, in order to be committed",
                    acc_pubkey
                );
                return Err(InstructionError::IllegalOwner);
            }

            // Validate committed account was scheduled by valid authority
            let acc_owner = *acc.borrow()?.owner();
            validate_commit_schedule_permissions(
                &invoke_context,
                &acc_owner,
                acc_pubkey,
                parent_program_id.as_ref(),
                &signers,
            )?;

            let account = acc.borrow()?;
            let committed_pubkey = try_remap_ata_to_eata(acc_pubkey, &account)
                .map_or(*acc_pubkey, |(pubkey, _)| pubkey);
            drop(account);

            if &committed_pubkey != acc_pubkey {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommit: remapping ATA {} -> eATA {}",
                    acc_pubkey,
                    committed_pubkey
                );
            }

            // Backwards-compat: merge duplicate accounts (by final restored pubkey).
            // Keep the first occurrence and ignore subsequent duplicates.
            if !seen_committed_pubkeys.insert(committed_pubkey) {
                continue;
            }

            accounts_to_commit.push((*acc_pubkey, acc));
        }
    }

    let commit_ids = accounts_to_commit
        .iter()
        .map(|(_, account)| reserve_commit_id(account))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(ref fee_vault) = magic_fee_vault {
        let fee = calculate_commit_fee_for_ids(commit_ids)?;
        charge_delegated_payer(&payer_account, fee_vault, fee)?;
    }

    // The payer or fee vault may also be committed. Copy their post-fee state,
    // before undelegation changes their owner and account mode.
    let committed_accounts = accounts_to_commit
        .iter()
        .map(|(pubkey, account)| {
            let account = account.borrow()?;
            Ok(CommittedAccount::from_account_shared(*pubkey, &account))
        })
        .collect::<Result<Vec<_>, InstructionError>>()?;
    if magic_fee_vault.is_none() && !opts.request_undelegation {
        // We validate commit nonces only for plain commits.
        // If accounts are undelegated we don't want to fail.
        check_commit_limits(&committed_accounts, invoke_context)?;
    }

    let context_acc = get_instruction_account_with_idx(transaction_context, MAGIC_CONTEXT_IDX)?;
    let mut context = MagicContext::deserialize(context_acc.borrow()?.data()).map_err(|err| {
        ic_msg!(
            invoke_context,
            "Failed to deserialize MagicContext: {}",
            err
        );
        InstructionError::GenericError
    })?;

    // Get next intent id
    let intent_id = context.next_intent_id();

    // It appears that in builtin programs `Clock::get` doesn't work as expected, thus
    // we have to get it directly from the sysvar cache.
    let clock = invoke_context.get_sysvar_cache().get_clock().map_err(|err| {
        ic_msg!(invoke_context, "Failed to get clock sysvar: {}", err);
        InstructionError::UnsupportedSysvar
    })?;
    let blockhash = invoke_context.environment_config.blockhash;

    let base_intent = if opts.request_undelegation {
        MagicBaseIntent::CommitFinalizeAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(committed_accounts),
            undelegate_action: UndelegateType::Standalone,
        })
    } else {
        MagicBaseIntent::CommitFinalize(CommitType::Standalone(committed_accounts))
    }
    .into();
    validate_intent_size(&base_intent).inspect_err(|_| {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: intent is too large to ever fit into transaction",
        );
    })?;

    if opts.request_undelegation {
        for (pubkey, account) in &accounts_to_commit {
            // Lock the local account only after capturing its original owner.
            // The transaction rolls this back if a later instruction fails.
            mark_account_as_undelegated(invoke_context, account)?;
            ic_msg!(
                invoke_context,
                "ScheduleCommit: Marking account {} as undelegating",
                pubkey
            );
        }
    }

    let scheduled_base_intent = ScheduledIntentBundle {
        intent_id,
        slot: clock.slot,
        blockhash,
        payer: *payer_pubkey,
        intent_bundle: base_intent,
    };

    context.add_scheduled_action(scheduled_base_intent);
    context.write_to(context_acc.borrow_mut()?.data_as_mut_slice())?;

    ic_msg!(invoke_context, "Scheduled commit with ID: {}", intent_id);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent outbox: {}",
        outbox_intent_pda(intent_id),
    );

    Ok(())
}
