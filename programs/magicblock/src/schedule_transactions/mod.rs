mod process_accept_scheduled_commits;
mod process_schedule_commit;
#[cfg(test)]
mod process_schedule_commit_tests;
mod process_schedule_intent_bundle;
mod process_scheduled_commit_sent;
pub(crate) mod transaction_scheduler;

use std::cell::RefCell;

use magicblock_core::intent::CommittedAccount;
use magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY;
pub(crate) use process_accept_scheduled_commits::*;
pub(crate) use process_schedule_commit::*;
pub(crate) use process_schedule_intent_bundle::process_schedule_intent_bundle;
pub use process_scheduled_commit_sent::{
    process_scheduled_commit_sent, register_scheduled_commit_sent, SentCommit,
};
use solana_account::AccountSharedData;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use crate::{
    magic_sys::{
        fetch_current_commit_nonces, COMMIT_LIMIT, COMMIT_LIMIT_ERR,
        MISSING_COMMIT_NONCE_ERR,
    },
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        get_writable_with_idx,
    },
};

pub(crate) fn check_commit_limits(
    commits: &[CommittedAccount],
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let mut nonces = fetch_current_commit_nonces(commits)?;
    let mut limit_exceeded = false;
    for account in commits {
        let nonce = nonces
            .remove(&account.pubkey)
            .ok_or(InstructionError::Custom(MISSING_COMMIT_NONCE_ERR))?;
        if nonce >= COMMIT_LIMIT {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: sponsored commit limit exceeded for account {}: current commit nonce {} reached the limit of {}. Undelegate and re-delegate the account or use a delegated account as the payer",
                account.pubkey,
                nonce,
                COMMIT_LIMIT
            );
            limit_exceeded = true;
        }
    }
    if limit_exceeded {
        Err(InstructionError::Custom(COMMIT_LIMIT_ERR))
    } else {
        Ok(())
    }
}

pub fn check_magic_context_id(
    invoke_context: &InvokeContext,
    idx: u16,
) -> Result<(), InstructionError> {
    let provided_magic_context = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        idx,
    )?;
    if !provided_magic_context.eq(&MAGIC_CONTEXT_PUBKEY) {
        ic_msg!(
            invoke_context,
            "ERR: invalid magic context account {}",
            provided_magic_context
        );
        return Err(InstructionError::MissingAccount);
    }

    Ok(())
}

pub(crate) fn magic_fee_vault_pubkey() -> Pubkey {
    let validator_authority = crate::validator::validator_authority_id();
    Pubkey::find_program_address(
        &[b"magic-fee-vault", validator_authority.as_ref()],
        &crate::utils::DELEGATION_PROGRAM_ID,
    )
    .0
}

/// Returns the fee vault account if the payer at `payer_idx` is delegated,
/// validating that the account at `fee_vault_idx` is the expected vault,
/// delegated, and writable. Returns `None` if the payer is not delegated.
///
/// Writability is checked eagerly: a delegated payer opts into the fee-charging
/// path, so a non-writable vault would cause a confusing failure at charge time
/// rather than a clear error upfront.
pub(crate) fn try_get_fee_vault<'a>(
    transaction_context: &'a TransactionContext,
    invoke_context: &InvokeContext,
    payer_idx: u16,
    fee_vault_idx: u16,
) -> Result<Option<&'a RefCell<AccountSharedData>>, InstructionError> {
    let payer_account =
        get_instruction_account_with_idx(transaction_context, payer_idx)?;
    if !payer_account.borrow().delegated() {
        return Ok(None);
    }

    let vault_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, fee_vault_idx)?;
    if vault_pubkey != &magic_fee_vault_pubkey() {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: invalid magic fee vault account {}",
            vault_pubkey
        );
        return Err(InstructionError::MissingAccount);
    }

    let vault_account =
        get_instruction_account_with_idx(transaction_context, fee_vault_idx)?;
    let is_vault_writable =
        get_writable_with_idx(transaction_context, fee_vault_idx)?;
    if !vault_account.borrow().delegated() || !is_vault_writable {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: magic fee vault must be writable and delegated"
        );
        return Err(InstructionError::IllegalOwner);
    }

    Ok(Some(vault_account))
}
