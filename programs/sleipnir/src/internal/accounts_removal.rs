use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};

use crate::{
    sleipnir_instruction::{into_transaction, SleipnirInstruction},
    validator_authority, validator_authority_id,
};
use crate::{
    traits::{AccountRemovalReason, AccountsRemover},
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
};
use lazy_static::lazy_static;
use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    account::{ReadableAccount, WritableAccount},
    hash::Hash,
    instruction::InstructionError,
    pubkey::Pubkey,
};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    transaction::Transaction,
};

// -----------------
// AccountsRemover
// -----------------
#[derive(Debug)]
pub struct PendingAccountRemoval {
    pub pubkey: Pubkey,
    pub reason: AccountRemovalReason,
}

#[derive(Clone)]
pub struct ValidatorAccountsRemover {
    accounts_pending_removal: Arc<RwLock<Vec<PendingAccountRemoval>>>,
}

impl Default for ValidatorAccountsRemover {
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

impl AccountsRemover for ValidatorAccountsRemover {
    fn request_accounts_removal(
        &self,
        pubkey: HashSet<Pubkey>,
        reason: AccountRemovalReason,
    ) {
        let mut accounts_pending_removal = self
            .accounts_pending_removal
            .write()
            .expect("accounts_pending_removal lock poisoned");
        for p in pubkey {
            accounts_pending_removal.push(PendingAccountRemoval {
                pubkey: p,
                reason: reason.clone(),
            });
        }
    }

    fn accounts_pending_removal(&self) -> HashSet<Pubkey> {
        self.accounts_pending_removal
            .read()
            .expect("accounts_pending_removal lock poisoned")
            .iter()
            .map(|x| x.pubkey)
            .collect()
    }
}

// -----------------
// Instruction to process removal from validator
// -----------------
pub fn process_accounts_pending_removal_transaction(
    accounts: HashSet<Pubkey>,
    recent_blockhash: Hash,
) -> Transaction {
    let ix = process_accounts_pending_removal_instruction(
        &crate::id(),
        &validator_authority_id(),
        accounts,
    );
    into_transaction(&validator_authority(), ix, recent_blockhash)
}

fn process_accounts_pending_removal_instruction(
    magic_block_program: &Pubkey,
    validator_authority: &Pubkey,
    accounts: HashSet<Pubkey>,
) -> Instruction {
    let mut account_metas = vec![
        AccountMeta::new_readonly(*magic_block_program, false),
        AccountMeta::new_readonly(*validator_authority, true),
    ];
    account_metas
        .extend(accounts.into_iter().map(|x| AccountMeta::new(x, false)));

    Instruction::new_with_bincode(
        *magic_block_program,
        &SleipnirInstruction::RemoveAccountsPendingRemoval,
        account_metas,
    )
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

    // All checks out, let's remove those accounts
    let pubkeys = ValidatorAccountsRemover::default()
        .accounts_pending_removal
        .read()
        .expect("accounts_pending_removal lock poisoned")
        .iter()
        .map(|x| x.pubkey)
        .collect::<HashSet<_>>();

    let mut to_remove = HashMap::new();
    let mut not_pending = HashSet::new();

    // For each account we remove, we transfer all its lamports to the validator authority
    for idx in ACCOUNTS_START..ix_accs_len {
        let acc_pubkey =
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc =
            get_instruction_account_with_idx(transaction_context, idx as u16)?;
        if pubkeys.contains(acc_pubkey) {
            to_remove.insert(*acc_pubkey, acc);
        } else {
            not_pending.insert(acc_pubkey);
        }
    }

    // The only place where accounts pending removal are taken out of the global
    // list is here.
    // Therefore we expect all accounts passed to still be in that list.
    // We clean them out of that list after we drain their lamports.
    if !not_pending.is_empty() {
        ic_msg!(
            invoke_context,
            "RemoveAccount ERR: Trying to remove accounts that aren't pending removal {:?}",
            not_pending
        );
        return Err(InstructionError::MissingAccount);
    }

    // Remove each account by draining its lamports
    let removed_pubkeys = to_remove.keys().cloned().collect::<HashSet<_>>();

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

    // Mark them as processed by removing from accounts pending removal
    {
        let remover = ValidatorAccountsRemover::default();
        let mut accounts_pending_removal = remover
            .accounts_pending_removal
            .write()
            .expect("accounts_pending_removal lock poisoned");
        accounts_pending_removal
            .retain(|x| !removed_pubkeys.contains(&x.pubkey));
        ic_msg!(
            invoke_context,
            "RemoveAccount: Removed accounts: {:?}. Remaining: {:?}",
            removed_pubkeys,
            accounts_pending_removal
        );
    }

    Ok(())
}

// TODO: test this
