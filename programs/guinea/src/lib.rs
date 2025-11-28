#![allow(unexpected_cfgs)]
use core::slice;

use magicblock_magic_program_api::{
    args::ScheduleTaskArgs, instruction::MagicBlockInstruction,
};
use serde::{Deserialize, Serialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::{self, ProgramResult},
    instruction::{AccountMeta, Instruction},
    log,
    program::{invoke, set_return_data},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};

entrypoint::entrypoint!(process_instruction);
declare_id!("GuineaeT4SgZ512pT3a5jfiG2gqBih6yVy2axJ2zo38C");

#[derive(Serialize, Deserialize)]
pub enum GuineaInstruction {
    ComputeBalances,
    PrintSizes,
    WriteByteToData(u8),
    Increment,
    Transfer(u64),
    Resize(usize),
    ScheduleTask(ScheduleTaskArgs),
    CancelTask(i64),
}

fn compute_balances(accounts: slice::Iter<AccountInfo>) {
    let total = accounts.map(|a| a.lamports()).sum::<u64>();
    set_return_data(&total.to_le_bytes());
}

fn resize_account(
    mut accounts: slice::Iter<AccountInfo>,
    size: usize,
) -> ProgramResult {
    let feepayer = next_account_info(&mut accounts)?;
    let account = next_account_info(&mut accounts)?;
    let rent = <Rent as Sysvar>::get()?;
    let new_account_balance = rent.minimum_balance(size) as i64;
    let delta = new_account_balance - account.try_lamports()? as i64;
    **account.try_borrow_mut_lamports()? = new_account_balance as u64;
    let feepayer_balance = feepayer.try_lamports()? as i64;
    **feepayer.try_borrow_mut_lamports()? = (feepayer_balance - delta) as u64;

    account.realloc(size, false)?;
    Ok(())
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
        let first =
            data.first_mut().ok_or(ProgramError::AccountDataTooSmall)?;
        *first = byte;
    }
    Ok(())
}

fn increment(accounts: slice::Iter<AccountInfo>) -> ProgramResult {
    for a in accounts {
        let mut data = a.try_borrow_mut_data()?;
        let first =
            data.first_mut().ok_or(ProgramError::AccountDataTooSmall)?;
        *first = first
            .checked_add(1)
            .ok_or(ProgramError::ArithmeticOverflow)?;
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

fn schedule_task(
    mut accounts: slice::Iter<AccountInfo>,
    args: ScheduleTaskArgs,
) -> ProgramResult {
    let magic_program_info = next_account_info(&mut accounts)?;
    let payer_info = next_account_info(&mut accounts)?;
    let counter_pda_info = next_account_info(&mut accounts)?;

    if magic_program_info.key != &magicblock_magic_program_api::ID {
        return Err(ProgramError::InvalidAccountData);
    }

    if !payer_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let ix = Instruction::new_with_bincode(
        magicblock_magic_program_api::ID,
        &MagicBlockInstruction::ScheduleTask(args),
        vec![
            AccountMeta::new(*payer_info.key, true),
            AccountMeta::new(*counter_pda_info.key, false),
        ],
    );

    invoke(&ix, &[payer_info.clone(), counter_pda_info.clone()])?;

    Ok(())
}

fn cancel_task(
    mut accounts: slice::Iter<AccountInfo>,
    task_id: i64,
) -> ProgramResult {
    let magic_program_info = next_account_info(&mut accounts)?;
    let payer_info = next_account_info(&mut accounts)?;

    if magic_program_info.key != &magicblock_magic_program_api::ID {
        return Err(ProgramError::InvalidAccountData);
    }

    if !payer_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let ix = Instruction::new_with_bincode(
        magicblock_magic_program_api::ID,
        &MagicBlockInstruction::CancelTask { task_id },
        vec![AccountMeta::new(*payer_info.key, true)],
    );

    invoke(&ix, &[payer_info.clone()])?;

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
        GuineaInstruction::Increment => increment(accounts)?,
        GuineaInstruction::Transfer(lamports) => transfer(accounts, lamports)?,
        GuineaInstruction::Resize(size) => resize_account(accounts, size)?,
        GuineaInstruction::ScheduleTask(request) => {
            schedule_task(accounts, request)?
        }
        GuineaInstruction::CancelTask(task_id) => {
            cancel_task(accounts, task_id)?
        }
    }
    Ok(())
}
