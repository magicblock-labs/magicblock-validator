use std::{collections::HashSet, sync::atomic::Ordering};

use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{
    account::ReadableAccount, instruction::InstructionError, pubkey::Pubkey,
};

use crate::{
    magic_context::{CommittedAccount, ScheduledCommit},
    schedule_transactions,
    schedule_transactions::{
        transaction_scheduler::TransactionScheduler, MESSAGE_ID,
    },
    utils::{
        account_actions::set_account_owner_to_delegation_program,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
        instruction_utils::InstructionUtils,
    },
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

    // Assert all accounts are owned by invoking program OR are signers
    // NOTE: we don't require PDAs to be signers as in our case verifying that the
    // program owning the PDAs invoked us via CPI is sufficient
    // Thus we can be `invoke`d unsigned and no seeds need to be provided
    let mut pubkeys: Vec<CommittedAccount> = Vec::new();
    for idx in COMMITTEES_START..ix_accs_len {
        let acc_pubkey =
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc =
            get_instruction_account_with_idx(transaction_context, idx as u16)?;

        {
            let acc_owner = *acc.borrow().owner();
            if parent_program_id != Some(&acc_owner)
                && !signers.contains(acc_pubkey)
            {
                return match parent_program_id {
                    None => {
                        ic_msg!(
                            invoke_context,
                            "ScheduleCommit ERR: failed to find parent program id"
                        );
                        Err(InstructionError::InvalidInstructionData)
                    }
                    Some(parent_id) => {
                        ic_msg!(
                            invoke_context,
                                "ScheduleCommit ERR: account {} needs to be owned by the invoking program {} or be a signer to be committed, but is owned by {}",
                                acc_pubkey, parent_id, acc_owner
                            );
                        Err(InstructionError::InvalidAccountOwner)
                    }
                };
            }
            #[allow(clippy::unnecessary_literal_unwrap)]
            pubkeys.push(CommittedAccount {
                pubkey: *acc_pubkey,
                owner: *parent_program_id.unwrap_or(&acc_owner),
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
            set_account_owner_to_delegation_program(acc);
            ic_msg!(
                invoke_context,
                "ScheduleCommit: account {} owner set to delegation program",
                acc_pubkey
            );
        }
    }

    // Determine id and slot
    let commit_id = MESSAGE_ID.fetch_add(1, Ordering::Relaxed);

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
    let commit_sent_transaction =
        InstructionUtils::scheduled_commit_sent(commit_id, blockhash);

    let commit_sent_sig = commit_sent_transaction.signatures[0];

    let scheduled_commit = ScheduledCommit {
        id: commit_id,
        slot: clock.slot,
        blockhash,
        accounts: pubkeys,
        payer: *payer_pubkey,
        commit_sent_transaction,
        request_undelegation: opts.request_undelegation,
    };

    // NOTE: this is only protected by all the above checks however if the
    // instruction fails for other reasons detected afterward then the commit
    // stays scheduled
    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    TransactionScheduler::schedule_action(
        invoke_context,
        context_acc,
        scheduled_commit.into(),
    )
    .map_err(|err| {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: failed to schedule commit: {}",
            err
        );
        InstructionError::GenericError
    })?;
    ic_msg!(invoke_context, "Scheduled commit with ID: {}", commit_id,);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent signature: {}",
        commit_sent_sig,
    );

    Ok(())
}
