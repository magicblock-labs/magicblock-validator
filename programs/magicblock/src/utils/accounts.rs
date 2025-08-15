#![allow(unused)] // most of these utilities will come in useful later
use std::cell::RefCell;

use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::{
    account::{Account, AccountSharedData, ReadableAccount, WritableAccount},
    account_info::{AccountInfo, IntoAccountInfo},
    instruction::{AccountMeta, InstructionError},
    pubkey::Pubkey,
    transaction_context::TransactionContext,
};

use crate::magic_scheduled_base_intent::ShortAccountMeta;

pub(crate) fn find_tx_index_of_instruction_account(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    not_found_msg: &str,
    pubkey: &Pubkey,
) -> Result<u16, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let idx = {
        let idx = ix_ctx
            .find_index_of_instruction_account(transaction_context, pubkey)
            .ok_or_else(|| {
                ic_msg!(invoke_context, "{}: {}", not_found_msg, pubkey);
                InstructionError::MissingAccount
            })?;
        ix_ctx.get_index_of_instruction_account_in_transaction(idx)
    }?;
    Ok(idx)
}

pub(crate) fn find_instruction_account<'a>(
    invoke_context: &'a InvokeContext,
    transaction_context: &'a TransactionContext,
    not_found_msg: &str,
    pubkey: &Pubkey,
) -> Result<&'a RefCell<AccountSharedData>, InstructionError> {
    let idx = find_tx_index_of_instruction_account(
        invoke_context,
        transaction_context,
        not_found_msg,
        pubkey,
    )?;
    let acc = transaction_context.get_account_at_index(idx)?;
    Ok(acc)
}

pub(crate) fn find_instruction_account_owner<'a>(
    invoke_context: &'a InvokeContext,
    transaction_context: &'a TransactionContext,
    not_found_msg: &str,
    pubkey: &Pubkey,
) -> Result<Pubkey, InstructionError> {
    let acc = find_instruction_account(
        invoke_context,
        transaction_context,
        not_found_msg,
        pubkey,
    )?;
    Ok(*acc.borrow().owner())
}

pub(crate) fn get_instruction_account_with_idx(
    transaction_context: &TransactionContext,
    idx: u16,
) -> Result<&RefCell<AccountSharedData>, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx = ix_ctx.get_index_of_instruction_account_in_transaction(idx)?;
    let acc = transaction_context.get_account_at_index(tx_idx)?;
    Ok(acc)
}

pub(crate) fn get_instruction_pubkey_and_account_with_idx(
    transaction_context: &TransactionContext,
    idx: u16,
) -> Result<(Pubkey, Account), InstructionError> {
    let acc_shared =
        get_instruction_account_with_idx(transaction_context, idx)?;
    let account = Account::from(acc_shared.borrow().clone());
    let pubkey = transaction_context.get_key_of_account_at_index(idx)?;
    Ok((*pubkey, account))
}

pub(crate) fn get_instruction_account_owner_with_idx(
    transaction_context: &TransactionContext,
    idx: u16,
) -> Result<Pubkey, InstructionError> {
    let acc = get_instruction_account_with_idx(transaction_context, idx)?;
    Ok(*acc.borrow().owner())
}

pub(crate) fn get_instruction_pubkey_with_idx(
    transaction_context: &TransactionContext,
    idx: u16,
) -> Result<&Pubkey, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx = ix_ctx.get_index_of_instruction_account_in_transaction(idx)?;
    let pubkey = transaction_context.get_key_of_account_at_index(tx_idx)?;
    Ok(pubkey)
}

pub(crate) fn get_instruction_account_short_meta_with_idx(
    transaction_context: &TransactionContext,
    idx: u16,
) -> Result<ShortAccountMeta, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx = ix_ctx.get_index_of_instruction_account_in_transaction(idx)?;

    let pubkey = *transaction_context.get_key_of_account_at_index(tx_idx)?;
    let is_writable = ix_ctx.is_instruction_account_writable(idx)?;
    Ok(ShortAccountMeta {
        pubkey,
        is_writable,
    })
}

pub(crate) fn debit_instruction_account_at_index(
    transaction_context: &TransactionContext,
    idx: u16,
    amount: u64,
) -> Result<(), InstructionError> {
    let account = get_instruction_account_with_idx(transaction_context, idx)?;
    let current_lamports = account.borrow().lamports();
    let new_lamports = current_lamports
        .checked_sub(amount)
        .ok_or(InstructionError::InsufficientFunds)?;
    account.borrow_mut().set_lamports(new_lamports);
    Ok(())
}

pub(crate) fn credit_instruction_account_at_index(
    transaction_context: &TransactionContext,
    idx: u16,
    amount: u64,
) -> Result<(), InstructionError> {
    let account = get_instruction_account_with_idx(transaction_context, idx)?;
    let current_lamports = account.borrow().lamports();
    let new_lamports = current_lamports
        .checked_add(amount)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    account.borrow_mut().set_lamports(new_lamports);
    Ok(())
}
