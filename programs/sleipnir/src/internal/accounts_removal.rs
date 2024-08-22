use std::{
    borrow::{Borrow, BorrowMut},
    collections::HashSet,
    sync::{Arc, RwLock},
};

use lazy_static::lazy_static;
use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    account::{Account, AccountSharedData, ReadableAccount, WritableAccount},
    instruction::InstructionError,
    pubkey::Pubkey,
};

use crate::utils::accounts::{
    get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
};

// -----------------
// AccountsRemover
// -----------------

#[derive(Clone)]
pub enum AccountRemovalReason {
    Undelegated,
}

pub struct PendingAccountRemoval {
    pub pubkey: Pubkey,
    pub reason: AccountRemovalReason,
}

#[derive(Clone)]
pub struct AccountsRemover {
    accounts_pending_removal: Arc<RwLock<Vec<PendingAccountRemoval>>>,
}

impl Default for AccountsRemover {
    fn default() -> Self {
        lazy_static! {
            static ref ACCOUNTS_PENDING_REMOVAL: Arc<RwLock<Vec<PendingAccountRemoval>>> =
                Default::default();
        }
        Self {
            accounts_pending_removal: ACCOUNTS_PENDING_REMOVAL.clone(),
        }
    }
}

// -----------------
// Processing removal from validator
// -----------------

pub fn process_remove_accounts_pending_removal(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    const PROGRAM_IDX: u16 = 0;
    const VALIDATOR_IDX: u16 = 1;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;
    const ACCOUNTS_START: usize = VALIDATOR_IDX as usize + 1;

    let program_id =
        get_instruction_pubkey_with_idx(transaction_context, PROGRAM_IDX)?;
    if program_id.ne(&crate::id()) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: Invalid program id '{}'",
            program_id
        );
        return Err(InstructionError::IncorrectProgramId);
    }

    // Assert validator identity matches
    let validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    let validator_authority_id = crate::validator_authority_id();
    if validator_pubkey != &validator_authority_id {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: provided validator account {} does not match validator identity {}",
            validator_pubkey, validator_authority_id
        );
        return Err(InstructionError::IncorrectAuthority);
    }

    // Assert validator authority signed
    if !signers.contains(&validator_authority_id) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: validator authority not found in signers"
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // All checks out, let's remove those accounts, shall we?
    let pubkeys = AccountsRemover::default()
        .accounts_pending_removal
        .read()
        .expect("accounts_pending_removal lock poisoned")
        .iter()
        .map(|x| x.pubkey)
        .collect::<HashSet<_>>();

    // The remaining accounts should include all pubkeys we are trying to remove
    let mut to_remove = Vec::new();

    // For each account we remove, we transfer all its lamports to the validator authority
    for idx in ACCOUNTS_START..ix_accs_len {
        let acc_pubkey =
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc =
            get_instruction_account_with_idx(transaction_context, idx as u16)?;
        if pubkeys.contains(acc_pubkey) {
            to_remove.push((*acc_pubkey, acc));
        }
    }

    // Ensure we were passed an account for each pubkey pending removal
    if !pubkeys.is_empty() {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: Missing account for pubkey(s) {:?}",
            pubkeys
        );
        return Err(InstructionError::MissingAccount);
    }

    // Remove each account by draining its lamports
    let to_remove_pubkeys =
        to_remove.iter().map(|x| x.0).collect::<HashSet<_>>();

    let mut total_drained_lamports = 0;
    for (_, acc) in to_remove.into_iter() {
        let current_lamports = acc.borrow().lamports();
        total_drained_lamports += current_lamports;
        acc.borrow_mut().set_lamports(0);
    }

    // Credit the drained lamports to the validator account
    let validator_acc =
        get_instruction_account_with_idx(transaction_context, VALIDATOR_IDX)?;
    let current_lamports = validator_acc.borrow().lamports();
    validator_acc
        .borrow_mut()
        .set_lamports(current_lamports + total_drained_lamports);

    // Mark them as processed
    {
        let remover = AccountsRemover::default();
        let mut accounts_pending_removal = remover
            .accounts_pending_removal
            .write()
            .expect("accounts_pending_removal lock poisoned");
        accounts_pending_removal
            .retain(|x| !to_remove_pubkeys.contains(&x.pubkey));
    }

    Ok(())
}

// TODO: test this
