use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

use crate::{
    common::{
        anchor_idl_seeds_with_bump, derive_anchor_idl_pda, derive_counter_pda,
        derive_shank_idl_pda, shank_idl_seeds_with_bump, IdlType,
    },
    instruction::MiniInstruction,
    state::{Counter, COUNTER_SIZE},
};

pub struct Processor;

impl Processor {
    pub fn process(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        instruction: &MiniInstruction,
    ) -> ProgramResult {
        msg!("Processing instruction: {:?}", instruction);
        match instruction {
            MiniInstruction::Init => Self::process_init(program_id, accounts),
            MiniInstruction::Increment => {
                Self::process_increment(program_id, accounts)
            }
            MiniInstruction::AddShankIdl(idl) => {
                Self::process_add_shank_idl(program_id, accounts, idl)
            }
            MiniInstruction::AddAnchorIdl(idl) => {
                Self::process_add_anchor_idl(program_id, accounts, idl)
            }
            MiniInstruction::LogMsg(msg_str) => Self::process_log_msg(msg_str),
        }
    }

    fn process_init(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
    ) -> ProgramResult {
        msg!("Processing Init instruction");
        let account_info_iter = &mut accounts.iter();
        let payer = next_account_info(account_info_iter)?;
        let counter_account = next_account_info(account_info_iter)?;
        let system_program = next_account_info(account_info_iter)?;

        // Verify the counter PDA
        let (expected_counter_pubkey, bump) =
            derive_counter_pda(&crate::id(), payer.key);
        if counter_account.key != &expected_counter_pubkey {
            return Err(ProgramError::InvalidSeeds);
        }

        // Create the counter account
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(COUNTER_SIZE);

        solana_program::program::invoke_signed(
            &system_instruction::create_account(
                payer.key,
                counter_account.key,
                required_lamports,
                COUNTER_SIZE as u64,
                program_id,
            ),
            &[
                payer.clone(),
                counter_account.clone(),
                system_program.clone(),
            ],
            &[&[b"counter", payer.key.as_ref(), &[bump]]],
        )?;

        // Initialize counter to 0
        let counter = Counter::new();
        let mut data = counter_account.try_borrow_mut_data()?;
        data[..8].copy_from_slice(&counter.to_bytes());

        msg!("Counter initialized");
        Ok(())
    }

    fn process_increment(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
    ) -> ProgramResult {
        msg!("Processing Inc instruction");
        let account_info_iter = &mut accounts.iter();
        let payer = next_account_info(account_info_iter)?;
        let counter_account = next_account_info(account_info_iter)?;

        // Verify payer is signer
        if !payer.is_signer {
            return Err(ProgramError::MissingRequiredSignature);
        }

        // Verify the counter PDA
        let (expected_counter_pubkey, _) =
            derive_counter_pda(&crate::id(), payer.key);
        if counter_account.key != &expected_counter_pubkey {
            return Err(ProgramError::InvalidSeeds);
        }

        // Verify account is owned by our program
        if counter_account.owner != program_id {
            return Err(ProgramError::IncorrectProgramId);
        }

        // Load, increment, and save counter
        let data = counter_account.try_borrow_data()?;
        let mut counter = Counter::from_bytes(&data)
            .map_err(|_| ProgramError::InvalidAccountData)?;
        drop(data);

        counter.increment();

        let mut data = counter_account.try_borrow_mut_data()?;
        data[..8].copy_from_slice(&counter.to_bytes());

        msg!("Counter incremented to {}", counter.count);
        Ok(())
    }

    fn process_add_shank_idl(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        idl: &[u8],
    ) -> ProgramResult {
        msg!("Processing AddShankIdl instruction");
        Self::process_idl_common(program_id, accounts, idl, IdlType::Shank)
    }

    fn process_add_anchor_idl(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        idl: &[u8],
    ) -> ProgramResult {
        msg!("Processing AddAnchorIdl instruction");
        Self::process_idl_common(program_id, accounts, idl, IdlType::Anchor)
    }

    fn process_idl_common(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        idl: &[u8],
        idl_type: IdlType,
    ) -> ProgramResult {
        use IdlType::*;
        let account_info_iter = &mut accounts.iter();
        let payer = next_account_info(account_info_iter)?;
        let idl_pda = next_account_info(account_info_iter)?;

        // 1. Create the IDL PDA
        let (idl_pubkey, bump) = match idl_type {
            Anchor => derive_anchor_idl_pda(program_id),
            Shank => derive_shank_idl_pda(program_id),
        };

        if idl_pda.key != &idl_pubkey {
            msg!(
                "Invalid IDL PDA: expected {}, got {}",
                idl_pubkey,
                idl_pda.key
            );
            return Err(ProgramError::InvalidSeeds);
        }

        // 2. Create account if it doesn't exist
        let size = idl.len();
        if idl_pda.data_is_empty() {
            let bump = [bump];
            let seeds = match idl_type {
                Anchor => anchor_idl_seeds_with_bump(program_id, &bump),
                Shank => shank_idl_seeds_with_bump(program_id, &bump),
            };
            let ix = system_instruction::create_account(
                payer.key,
                idl_pda.key,
                Rent::get()?.minimum_balance(size),
                size as u64,
                program_id,
            );
            invoke_signed(&ix, &[payer.clone(), idl_pda.clone()], &[&seeds])?;
        }
        // 3. We don't support resizing
        else if idl_pda.data_len() != size {
            msg!(
                "IDL PDA has unexpected size: expected {}, got {}",
                size,
                idl_pda.data_len()
            );
            return Err(ProgramError::InvalidAccountData);
        }

        // 2. Write the IDL data to the PDA
        idl_pda.data.borrow_mut()[..size].copy_from_slice(idl);

        Ok(())
    }

    fn process_log_msg(msg_str: &str) -> ProgramResult {
        let suffix = option_env!("LOG_MSG_SUFFIX").unwrap_or("");
        msg!("LogMsg: {}{}", msg_str, suffix);
        Ok(())
    }
}
