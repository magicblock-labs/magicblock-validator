#![allow(unexpected_cfgs)]
use core::slice;

use serde::{Deserialize, Serialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::{self, ProgramResult},
    log,
    program::set_return_data,
    program_error::ProgramError,
    pubkey::Pubkey,
};

entrypoint::entrypoint!(process_instruction);
declare_id!("GuineaeT4SgZ512pT3a5jfiG2gqBih6yVy2axJ2zo38C");

#[derive(Serialize, Deserialize)]
pub enum GuineaInstruction {
    ComputeBalances,
    PrintSizes,
    WriteByteToData(u8),
    Transfer(u64),
}

fn compute_balances(accounts: slice::Iter<AccountInfo>) {
    let total = accounts.map(|a| a.lamports()).sum::<u64>();
    set_return_data(&total.to_le_bytes());
}

fn print_sizes(accounts: slice::Iter<AccountInfo>) {
    for a in accounts {
        log::msg!("Account {} has data size of {} bytes", a.key, a.data_len());
    }
}

fn write_byte_to_data(
    accounts: slice::Iter<AccountInfo>,
    byte: u8,
) -> ProgramResult {
    for a in accounts {
        let mut data = a.try_borrow_mut_data()?;
        let first = data
            .first_mut()
            .ok_or(ProgramError::AccountDataTooSmall)?;
        *first = byte;
    }
    Ok(())
}

fn transfer(
    mut accounts: slice::Iter<AccountInfo>,
    lamports: u64,
) -> ProgramResult {
    let sender = next_account_info(&mut accounts)?;
    let recipient = next_account_info(&mut accounts)?;
    let mut from_lamports = sender.try_borrow_mut_lamports()?;
    let mut to_lamports = recipient.try_borrow_mut_lamports()?;
    **from_lamports = from_lamports
        .checked_sub(lamports)
        .ok_or(ProgramError::InsufficientFunds)?;
    **to_lamports = to_lamports
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    log::msg!(
        "Sent {} lamport from {} to {}",
        lamports,
        sender.key,
        recipient.key
    );
    Ok(())
}

fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction: GuineaInstruction = bincode::deserialize(instruction_data)
        .map_err(|err| {
            log::msg!(
                "failed to bincode deserialize instruction data: {}",
                err
            );
            ProgramError::InvalidInstructionData
        })?;
    let accounts = accounts.iter();
    match instruction {
        GuineaInstruction::ComputeBalances => compute_balances(accounts),
        GuineaInstruction::PrintSizes => print_sizes(accounts),
        GuineaInstruction::WriteByteToData(byte) => {
            write_byte_to_data(accounts, byte)?
        }
        GuineaInstruction::Transfer(lamports) => transfer(accounts, lamports)?,
    }
    Ok(())
}
