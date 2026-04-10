#![allow(unused)] // most of these utilities will come in useful later

use magicblock_magic_program_api::args::ShortAccountMeta;
use solana_account::{
    Account, AccountSharedData, ReadableAccount, WritableAccount,
};
use solana_account_info::{AccountInfo, IntoAccountInfo};
use solana_instruction::{error::InstructionError, AccountMeta};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::{
    transaction_accounts::{AccountRef, AccountRefMut},
    TransactionContext,
};

pub(crate) struct InstructionAccount<'a, 'ix_data> {
    transaction_context: &'a TransactionContext<'ix_data>,
    instruction_idx: u16,
    tx_idx: u16,
}

impl<'a, 'ix_data> InstructionAccount<'a, 'ix_data> {
    pub(crate) fn to_account_shared_data(
        &self,
    ) -> Result<AccountSharedData, InstructionError> {
        Ok(self.borrow()?.to_account_shared_data())
    }

    pub(crate) fn borrow(&self) -> Result<AccountRef<'_>, InstructionError> {
        self.transaction_context
            .accounts()
            .try_borrow(self.tx_idx)
            .map_err(|_| InstructionError::AccountBorrowFailed)
    }

    pub(crate) fn borrow_mut(
        &self,
    ) -> Result<AccountRefMut<'_>, InstructionError> {
        self.transaction_context
            .accounts()
            .try_borrow_mut(self.tx_idx)
            .map_err(|_| InstructionError::AccountBorrowFailed)
    }
}

pub(crate) fn find_instruction_account<'a, 'ix_data>(
    invoke_context: &'a InvokeContext,
    transaction_context: &'a TransactionContext<'ix_data>,
    not_found_msg: &str,
    pubkey: &Pubkey,
) -> Result<InstructionAccount<'a, 'ix_data>, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let Some(tx_idx) = transaction_context.find_index_of_account(pubkey) else {
        ic_msg!(invoke_context, "{}: {}", not_found_msg, pubkey);
        return Err(InstructionError::MissingAccount);
    };
    let instruction_idx = ix_ctx.get_index_of_account_in_instruction(tx_idx)?;
    Ok(InstructionAccount {
        transaction_context,
        instruction_idx,
        tx_idx,
    })
}

pub(crate) fn find_instruction_account_owner<'a>(
    invoke_context: &'a InvokeContext,
    transaction_context: &'a TransactionContext<'_>,
    not_found_msg: &str,
    pubkey: &Pubkey,
) -> Result<Pubkey, InstructionError> {
    let acc = find_instruction_account(
        invoke_context,
        transaction_context,
        not_found_msg,
        pubkey,
    )?;
    Ok(*acc.to_account_shared_data()?.owner())
}

pub(crate) fn get_instruction_account_with_idx<'a, 'ix_data>(
    transaction_context: &'a TransactionContext<'ix_data>,
    idx: u16,
) -> Result<InstructionAccount<'a, 'ix_data>, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx = ix_ctx.get_index_of_instruction_account_in_transaction(idx)?;
    Ok(InstructionAccount {
        transaction_context,
        instruction_idx: idx,
        tx_idx,
    })
}

pub(crate) fn get_instruction_pubkey_and_account_with_idx(
    transaction_context: &TransactionContext<'_>,
    idx: u16,
) -> Result<(Pubkey, Account), InstructionError> {
    let account_ref =
        get_instruction_account_with_idx(transaction_context, idx)?;
    let account = account_ref.borrow()?;
    let account = Account::from(account.to_account_shared_data());
    let pubkey =
        transaction_context.get_key_of_account_at_index(account_ref.tx_idx)?;
    Ok((*pubkey, account))
}

pub(crate) fn get_instruction_account_owner_with_idx(
    transaction_context: &TransactionContext<'_>,
    idx: u16,
) -> Result<Pubkey, InstructionError> {
    let acc = get_instruction_account_with_idx(transaction_context, idx)?;
    Ok(*acc.to_account_shared_data()?.owner())
}

pub(crate) fn get_instruction_pubkey_with_idx<'a>(
    transaction_context: &'a TransactionContext<'_>,
    idx: u16,
) -> Result<&'a Pubkey, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let tx_idx = ix_ctx.get_index_of_instruction_account_in_transaction(idx)?;
    let pubkey = transaction_context.get_key_of_account_at_index(tx_idx)?;
    Ok(pubkey)
}

pub(crate) fn get_writable_with_idx(
    transaction_context: &TransactionContext<'_>,
    idx: u16,
) -> Result<bool, InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let writable = ix_ctx.is_instruction_account_writable(idx)?;
    Ok(writable)
}

pub(crate) fn get_instruction_account_short_meta_with_idx(
    transaction_context: &TransactionContext<'_>,
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
    transaction_context: &TransactionContext<'_>,
    idx: u16,
    amount: u64,
) -> Result<(), InstructionError> {
    let account = get_instruction_account_with_idx(transaction_context, idx)?;
    let current_lamports = account.borrow()?.lamports();
    let new_lamports = current_lamports
        .checked_sub(amount)
        .ok_or(InstructionError::InsufficientFunds)?;
    account.borrow_mut()?.set_lamports(new_lamports);
    Ok(())
}

pub(crate) fn credit_instruction_account_at_index(
    transaction_context: &TransactionContext<'_>,
    idx: u16,
    amount: u64,
) -> Result<(), InstructionError> {
    let account = get_instruction_account_with_idx(transaction_context, idx)?;
    let current_lamports = account.borrow()?.lamports();
    let new_lamports = current_lamports
        .checked_add(amount)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    account.borrow_mut()?.set_lamports(new_lamports);
    Ok(())
}
