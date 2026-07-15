mod process_add_action_callback;
mod process_schedule_cloned_undelegation;
mod process_schedule_commit;
#[cfg(test)]
mod process_schedule_commit_tests;
mod process_schedule_intent_bundle;
pub(crate) mod transaction_scheduler;

use std::sync::Arc;

use magicblock_core::intent::types::CommittedAccount;
use magicblock_magic_program_api::{
    pda::CALLBACK_SIGNER, MAGIC_CONTEXT_PUBKEY,
};
pub(crate) use process_add_action_callback::process_add_action_callback;
pub(crate) use process_schedule_cloned_undelegation::process_schedule_cloned_account_undelegation;
pub(crate) use process_schedule_commit::*;
pub(crate) use process_schedule_intent_bundle::process_schedule_intent_bundle;
use solana_clock::Clock;
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

pub use crate::outbox_intent::process_scheduled_commit_sent::{
    register_scheduled_commit_sent, SentCommit,
};
pub(crate) use crate::intent_bundles::{
    process_accept_scheduled_commits::*, process_execute_callback::*,
};
use crate::{
    magic_sys::{
        fetch_current_commit_nonces, COMMIT_LIMIT, COMMIT_LIMIT_ERR,
        MISSING_COMMIT_NONCE_ERR,
    },
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        get_writable_with_idx, InstructionAccount,
    },
    validator::effective_validator_authority_id,
};

pub(crate) const PAYER_IDX: u16 = 0;
pub(crate) const MAGIC_CONTEXT_IDX: u16 = PAYER_IDX + 1;
#[cfg(test)]
pub(crate) const ACCOUNTS_OFFSET: usize = MAGIC_CONTEXT_IDX as usize + 1;

#[cfg(not(test))]
fn get_parent_program_id(
    invoke_context: &mut InvokeContext,
) -> Result<Option<Pubkey>, InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;
    let frames = crate::utils::instruction_context_frames::InstructionContextFrames::try_from(transaction_context)?;
    let parent_program_id =
        frames.find_program_id_of_parent_of_current_instruction();

    ic_msg!(
        invoke_context,
        "ScheduleCommit: parent program id: {}",
        parent_program_id
            .map_or_else(|| "None".to_string(), |id| id.to_string())
    );

    Ok(parent_program_id.copied())
}

#[cfg(test)]
fn get_parent_program_id(
    invoke_context: &mut InvokeContext,
) -> Result<Option<Pubkey>, InstructionError> {
    use solana_account::ReadableAccount;
    let transaction_context = &*invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Action-only bundles may legitimately contain only payer + magic context.
    // In unit tests we cannot recover CPI frames, so use a stable placeholder
    // instead of failing before we can exercise the scheduling logic.
    if ix_ctx.get_number_of_instruction_accounts() as usize <= ACCOUNTS_OFFSET {
        return Ok(Some(crate::id()));
    }

    use crate::utils::accounts::get_instruction_account_with_idx;

    let first_committee_owner = *get_instruction_account_with_idx(
        transaction_context,
        ACCOUNTS_OFFSET as u16,
    )?
    .borrow()?
    .owner();

    Ok(Some(first_committee_owner))
}

pub(crate) fn get_clock(
    invoke_context: &mut InvokeContext,
) -> Result<Arc<Clock>, InstructionError> {
    invoke_context
        .get_sysvar_cache()
        .get_clock()
        .map_err(|err| {
            ic_msg!(invoke_context, "Failed to get clock sysvar: {}", err);
            InstructionError::UnsupportedSysvar
        })
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

pub(crate) fn magic_fee_vault_pubkey() -> Pubkey {
    let validator_authority = crate::validator::validator_authority_id();
    Pubkey::find_program_address(
        &[b"magic-fee-vault", validator_authority.as_ref()],
        &crate::utils::DELEGATION_PROGRAM_ID,
    )
    .0
}

/// Returns the fee vault account if the payer at `payer_idx` uses the
/// fee-vault path, validating that the account at `fee_vault_idx` is the
/// expected vault, delegated, and writable. Returns `None` otherwise.
///
/// Writability is checked eagerly: a payer on the fee-charging path would
/// otherwise fail later with a less clear error.
pub(crate) fn try_get_fee_vault<'a, 'ix_data>(
    transaction_context: &'a TransactionContext<'ix_data>,
    invoke_context: &InvokeContext,
    payer_idx: u16,
    fee_vault_idx: u16,
) -> Result<Option<InstructionAccount<'a, 'ix_data>>, InstructionError> {
    let payer_account =
        get_instruction_account_with_idx(transaction_context, payer_idx)?;
    let payer_requires_fee_vault = {
        let payer = payer_account.to_account_shared_data()?;
        payer.delegated() && !payer.confined()
    };
    if !payer_requires_fee_vault {
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
    if !vault_account.to_account_shared_data()?.delegated()
        || !is_vault_writable
    {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: magic fee vault must be writable and delegated"
        );
        return Err(InstructionError::IllegalOwner);
    }

    Ok(Some(vault_account))
}

/// Assert that the callback instructions do not have signers aside from the callback signer
/// Assert they don't use the validator either
pub(crate) fn validate_callback_accounts(
    invoke_context: &&mut InvokeContext,
    accounts_meta: &[AccountMeta],
    err_prefix: &str,
) -> Result<(), InstructionError> {
    for AccountMeta {
        pubkey,
        is_signer,
        is_writable,
    } in accounts_meta
    {
        if *is_writable && pubkey == &CALLBACK_SIGNER {
            ic_msg!(
                invoke_context,
                "{}: the callback signer PDA cannot be a writable account in callbacks",
                err_prefix,
            );
            return Err(InstructionError::Immutable);
        }

        if *is_signer && pubkey != &CALLBACK_SIGNER {
            ic_msg!(
                invoke_context,
                "{}: only the callback signer PDA can be a signer in callbacks (invalid signer: '{}')",
                err_prefix,
                pubkey,
            );
            return Err(InstructionError::IncorrectAuthority);
        }

        if pubkey == &effective_validator_authority_id() {
            ic_msg!(
                invoke_context,
                "{}: the validator authority cannot be used in callbacks",
                err_prefix,
            );
            return Err(InstructionError::IncorrectAuthority);
        }
    }
    Ok(())
}
