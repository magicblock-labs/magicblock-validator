use std::collections::HashSet;

use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{
    account::{Account, ReadableAccount},
    account_utils::StateMut,
    instruction::InstructionError,
    pubkey::Pubkey,
};

use crate::{
    magic_scheduled_base_intent::{
        validate_commit_schedule_rights, CommitAndUndelegate, CommitType,
        CommittedAccount, MagicBaseIntent, ScheduledBaseIntent, UndelegateType,
    },
    schedule_transactions,
    utils::{
        account_actions::mark_account_as_undelegated,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
            get_writable_with_idx,
        },
        instruction_utils::InstructionUtils,
    },
    MagicContext,
};

#[derive(Default)]
pub(crate) struct ProcessScheduleCommitOptions {
    pub request_undelegation: bool,
}

pub(crate) fn process_schedule_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    opts: ProcessScheduleCommitOptions,
) -> Result<(), InstructionError> {
    const PAYER_IDX: u16 = 0;
    const MAGIC_CONTEXT_IDX: u16 = PAYER_IDX + 1;

    schedule_transactions::check_magic_context_id(
        invoke_context,
        MAGIC_CONTEXT_IDX,
    )?;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;
    const COMMITTEES_START: usize = MAGIC_CONTEXT_IDX as usize + 1;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    // Assert enough accounts
    if ix_accs_len <= COMMITTEES_START {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: not enough accounts to schedule commit ({}), need payer, signing program an account for each pubkey to be committed",
            ix_accs_len
        );
        return Err(InstructionError::NotEnoughAccountKeys);
    }

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

    //
    // Get the program_id of the parent instruction that invoked this one via CPI
    //

    // We cannot easily simulate the transaction being invoked via CPI
    // from the owning program during unit tests
    // Instead the integration tests ensure that this works as expected
    #[cfg(not(test))]
    let frames = crate::utils::instruction_context_frames::InstructionContextFrames::try_from(transaction_context)?;

    // During unit tests we assume the first committee has the correct program ID
    #[cfg(test)]
    let first_committee_owner = {
        *get_instruction_account_with_idx(
            transaction_context,
            COMMITTEES_START as u16,
        )?
        .borrow()
        .owner()
    };

    #[cfg(not(test))]
    let parent_program_id = {
        let parent_program_id =
            frames.find_program_id_of_parent_of_current_instruction();

        ic_msg!(
            invoke_context,
            "ScheduleCommit: parent program id: {}",
            parent_program_id
                .map_or_else(|| "None".to_string(), |id| id.to_string())
        );

        parent_program_id
    };

    #[cfg(test)]
    let parent_program_id = Some(&first_committee_owner);

    // Assert all accounts are delegated, owned by invoking program OR are signers
    // Also works if the validator authority is a signer
    // NOTE: we don't require PDAs to be signers as in our case verifying that the
    // program owning the PDAs invoked us via CPI is sufficient
    // Thus we can be `invoke`d unsigned and no seeds need to be provided
    let mut committed_accounts: Vec<CommittedAccount> = Vec::new();
    for idx in COMMITTEES_START..ix_accs_len {
        let acc_pubkey =
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc =
            get_instruction_account_with_idx(transaction_context, idx as u16)?;
        {
            if opts.request_undelegation {
                // Check if account is writable and also undelegated
                // SVM doesn't check delegated, so we need to do extra checks here
                // Otherwise account could be undelegated twice
                let acc_writable =
                    get_writable_with_idx(transaction_context, idx as u16)?;
                if !acc_writable || !acc.borrow().delegated() {
                    ic_msg!(
                        invoke_context,
                        "ScheduleCommit ERR: account {} is required to be writable and delegated in order to be undelegated",
                        acc_pubkey
                    );
                    return Err(InstructionError::ReadonlyDataModified);
                }
            } else if !acc.borrow().delegated() {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommit ERR: account {} is required to be delegated to the current validator, in order to be committed",
                    acc_pubkey
                );
                return Err(InstructionError::IllegalOwner);
            };

            // Validate committed account was scheduled by valid authority
            let acc_owner = *acc.borrow().owner();
            validate_commit_schedule_rights(
                &invoke_context,
                acc_pubkey,
                &acc_owner,
                parent_program_id,
                &signers,
            )?;

            let mut account: Account = acc.borrow().to_owned().into();
            account.owner = parent_program_id.cloned().unwrap_or(account.owner);

            #[allow(clippy::unnecessary_literal_unwrap)]
            committed_accounts.push(CommittedAccount {
                pubkey: *acc_pubkey,
                account,
            });
        }

        if opts.request_undelegation {
            // If the account is scheduled to be undelegated then we need to lock it
            // immediately in order to prevent the following actions:
            // - writes to the account
            // - scheduling further commits for this account
            //
            // Setting the owner will prevent both, since in both cases the _actual_
            // owner program needs to sign for the account which is not possible at
            // that point
            // NOTE: this owner change only takes effect if the transaction which
            // includes this instruction succeeds.
            //
            // We also set the undelegating flag on the account in order to detect
            // undelegations for which we miss updates
            mark_account_as_undelegated(acc);
            ic_msg!(
                invoke_context,
                "ScheduleCommit: Marking account {} as undelegating",
                acc_pubkey
            );
        }
    }

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

    // It appears that in builtin programs `Clock::get` doesn't work as expected, thus
    // we have to get it directly from the sysvar cache.
    let clock =
        invoke_context
            .get_sysvar_cache()
            .get_clock()
            .map_err(|err| {
                ic_msg!(invoke_context, "Failed to get clock sysvar: {}", err);
                InstructionError::UnsupportedSysvar
            })?;
    let blockhash = invoke_context.environment_config.blockhash;
    let action_sent_transaction =
        InstructionUtils::scheduled_commit_sent(intent_id, blockhash);
    let commit_sent_sig = action_sent_transaction.signatures[0];

    let base_intent = if opts.request_undelegation {
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(committed_accounts),
            undelegate_action: UndelegateType::Standalone,
        })
    } else {
        MagicBaseIntent::Commit(CommitType::Standalone(committed_accounts))
    };
    let scheduled_base_intent = ScheduledBaseIntent {
        id: intent_id,
        slot: clock.slot,
        blockhash,
        action_sent_transaction,
        payer: *payer_pubkey,
        base_intent,
    };

    context.add_scheduled_action(scheduled_base_intent);
    context_data.set_state(&context)?;

    ic_msg!(invoke_context, "Scheduled commit with ID: {}", intent_id);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent signature: {}",
        commit_sent_sig,
    );

    Ok(())
}
