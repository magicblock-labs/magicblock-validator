use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    clock::Clock, fee_calculator::DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE,
    instruction::InstructionError, pubkey::Pubkey, sysvar::Sysvar,
    transaction_context::TransactionContext,
};

use crate::{
    schedule_transactions::transaction_scheduler::TransactionScheduler,
    utils::accounts::{
        credit_instruction_account_at_index,
        debit_instruction_account_at_index, find_instruction_account_owner,
        get_instruction_pubkey_with_idx,
    },
};

use super::transaction_scheduler::ScheduledCommit;

pub(crate) fn process_schedule_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    pubkeys: Vec<Pubkey>,
) -> Result<(), InstructionError> {
    static ID: AtomicU64 = AtomicU64::new(0);

    const PAYER_IDX: u16 = 0;
    const PROGRAM_IDX: u16 = 1;
    const VALIDATOR_IDX: u16 = 2;

    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;

    let committees_len = pubkeys.len();
    const SIGNERS_LEN: usize = 2;
    const AUTHORITIES_LEN: usize = 1;

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
    if ix_accs_len < SIGNERS_LEN + AUTHORITIES_LEN + committees_len {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: not enough accounts to schedule commit ({}), need payer, signing program an account for each pubkey to be committed",
            ix_accs_len
        );
        return Err(InstructionError::NotEnoughAccountKeys);
    }

    // Assert signers
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

    let owner_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, PROGRAM_IDX)?;
    if !signers.contains(owner_pubkey) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: owner pubkey {} not in signers",
            owner_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }
    // Assert validator identity matches
    let _validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    // TODO(thlorenz): @@@ access validator identity and compare pubkey
    // if validator_pubkey != validator_identity { ...

    // Assert all committees are owned by the invoking program
    for pubkey in &pubkeys {
        let acc_owner = find_instruction_account_owner(
            invoke_context,
            transaction_context,
            "ScheduleCommit ERR: account to commit not found",
            pubkey,
        )?;
        if owner_pubkey != &acc_owner {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: account {} needs to be owned by invoking program {} to be committed, but is owned by {}",
                pubkey, owner_pubkey, acc_owner
            );
            return Err(InstructionError::IllegalOwner);
        }
    }

    // Determine id and slot
    let id = ID.fetch_add(1, Ordering::Relaxed);
    let clock: Clock = Clock::get().map_err(|err| {
        ic_msg!(invoke_context, "Failed to get clock sysvar: {}", err);
        InstructionError::UnsupportedSysvar
    })?;

    // Deduct lamports from payer to pay for transaction
    // For now we assume that chain cost match the defaults
    // We may have to charge more here if we want to pay extra to ensure the
    // transacotin lands.
    let tx_cost = DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE;
    debit_instruction_account_at_index(
        transaction_context,
        PAYER_IDX,
        tx_cost,
    )?;
    credit_instruction_account_at_index(
        transaction_context,
        VALIDATOR_IDX,
        tx_cost,
    )?;

    let scheduled_commit = ScheduledCommit {
        id,
        slot: clock.slot,
        accounts: pubkeys,
    };

    TransactionScheduler::default().schedule_commit(scheduled_commit);
    ic_msg!(invoke_context, "Scheduled commit: {}", id,);

    Ok(())
}
