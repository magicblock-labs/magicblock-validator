use borsh::{to_vec, BorshDeserialize};
use ephemeral_rollups_sdk::{
    consts::{EXTERNAL_UNDELEGATE_DISCRIMINATOR, MAGIC_PROGRAM_ID},
    cpi::{
        delegate_account, undelegate_account, DelegateAccounts, DelegateConfig,
    },
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

use crate::{
    instruction::{
        create_increment_counter_error_ix, create_increment_counter_ix,
        create_increment_counter_signed_ix, CancelArgs, DelegateArgs,
        ScheduleArgs, ScheduleTaskInstruction,
    },
    magic_program::{CancelTaskArgs, ScheduleTaskArgs},
    state::Counter,
    utils::assert_keys_equal,
};

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if instruction_data.len() >= EXTERNAL_UNDELEGATE_DISCRIMINATOR.len() {
        let (disc, seeds_data) =
            instruction_data.split_at(EXTERNAL_UNDELEGATE_DISCRIMINATOR.len());

        if disc == EXTERNAL_UNDELEGATE_DISCRIMINATOR {
            return process_undelegate_request(accounts, seeds_data);
        }
    }

    let ix = ScheduleTaskInstruction::try_from_slice(instruction_data)?;
    msg!("Processing instruction {:?}", ix);
    use ScheduleTaskInstruction::*;
    match ix {
        InitCounter => process_init_counter(program_id, accounts),
        Schedule(args) => process_schedule_task(accounts, args),
        Cancel(args) => process_cancel_task(accounts, args),
        IncrementCounter => process_increment_counter(accounts),
        IncrementCounterSigned => process_increment_counter_signed(accounts),
        IncrementCounterError => process_increment_counter_error(),
        Delegate(args) => process_delegate(accounts, &args),
    }?;
    Ok(())
}

fn process_init_counter(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> ProgramResult {
    msg!("InitCounter");

    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;

    let (counter_pda, bump) = Counter::pda(payer_info.key);
    assert_keys_equal(counter_pda_info.key, &counter_pda, || {
        format!(
            "Invalid counter PDA {}, should be {}",
            counter_pda_info.key, counter_pda
        )
    })?;

    let bump = &[bump];
    let seeds = Counter::seeds_with_bump(payer_info.key, bump);

    let counter = Counter::default();

    let counter_data = to_vec(&counter)?;
    let size = counter_data.len();
    let ix = system_instruction::create_account(
        payer_info.key,
        counter_pda_info.key,
        Rent::get()?.minimum_balance(size),
        size as u64,
        program_id,
    );
    invoke_signed(
        &ix,
        &[payer_info.clone(), counter_pda_info.clone()],
        &[&seeds],
    )?;

    counter_pda_info.data.borrow_mut()[..size].copy_from_slice(&counter_data);

    Ok(())
}

fn process_schedule_task(
    accounts: &[AccountInfo],
    args: ScheduleArgs,
) -> ProgramResult {
    msg!("ScheduleTask");

    let account_info_iter = &mut accounts.iter();
    let _magic_program_info = next_account_info(account_info_iter)?;
    let payer_info = next_account_info(account_info_iter)?;
    let task_context_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;

    let (counter_pda, bump) = Counter::pda(payer_info.key);
    if counter_pda_info.key.ne(&counter_pda) {
        msg!(
            "Invalid counter PDA {}, should be {}",
            counter_pda_info.key,
            counter_pda
        );
        return Err(ProgramError::InvalidSeeds);
    }
    let bump = &[bump];
    let seeds = Counter::seeds_with_bump(payer_info.key, bump);

    let mut discriminator = vec![5_u8, 0, 0, 0];
    let args = ScheduleTaskArgs {
        task_id: args.task_id,
        period_millis: args.period_millis,
        n_executions: args.n_executions,
        instructions: vec![match (args.error, args.signer) {
            (true, false) => create_increment_counter_error_ix(*payer_info.key),
            (false, true) => {
                create_increment_counter_signed_ix(*payer_info.key)
            }
            _ => create_increment_counter_ix(*payer_info.key),
        }],
    };
    discriminator.extend_from_slice(&bincode::serialize(&args).map_err(
        |err| {
            msg!("ERROR: failed to serialize args {:?}", err);
            ProgramError::InvalidArgument
        },
    )?);

    let ix = Instruction::new_with_bytes(
        MAGIC_PROGRAM_ID,
        &discriminator,
        vec![
            AccountMeta::new(*payer_info.key, true),
            AccountMeta::new(*task_context_info.key, false),
            AccountMeta::new_readonly(*counter_pda_info.key, true),
        ],
    );

    invoke_signed(
        &ix,
        &[
            payer_info.clone(),
            task_context_info.clone(),
            counter_pda_info.clone(),
        ],
        &[&seeds],
    )?;

    Ok(())
}

fn process_cancel_task(
    accounts: &[AccountInfo],
    args: CancelArgs,
) -> ProgramResult {
    msg!("CancelTask");

    let account_info_iter = &mut accounts.iter();
    let _magic_program_info = next_account_info(account_info_iter)?;
    let payer_info = next_account_info(account_info_iter)?;
    let task_context_info = next_account_info(account_info_iter)?;

    let mut discriminator = vec![6_u8, 0, 0, 0];
    let args = CancelTaskArgs {
        task_id: args.task_id,
    };
    discriminator.extend_from_slice(&bincode::serialize(&args).map_err(
        |err| {
            msg!("ERROR: failed to serialize args {:?}", err);
            ProgramError::InvalidArgument
        },
    )?);

    let ix = Instruction::new_with_bytes(
        MAGIC_PROGRAM_ID,
        &discriminator,
        vec![
            AccountMeta::new(*payer_info.key, true),
            AccountMeta::new(*task_context_info.key, false),
        ],
    );

    invoke(&ix, &[payer_info.clone(), task_context_info.clone()])?;

    Ok(())
}

fn process_increment_counter(accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;

    if !payer_info.is_signer {
        msg!("Payer is not signer");
        return Err(ProgramError::InvalidAccountData);
    }

    let (counter_pda, _) = Counter::pda(payer_info.key);
    assert_keys_equal(counter_pda_info.key, &counter_pda, || {
        format!(
            "Invalid counter PDA {}, should be {}",
            counter_pda_info.key, counter_pda
        )
    })?;
    if !counter_pda_info.is_writable {
        msg!("Counter PDA is not writable");
        return Err(ProgramError::InvalidAccountData);
    }

    let mut counter = Counter::try_from_slice(&counter_pda_info.data.borrow())?;

    counter.count += 1;

    let size = counter_pda_info.data_len();
    let counter_data = to_vec(&counter)?;
    counter_pda_info.data.borrow_mut()[..size].copy_from_slice(&counter_data);

    Ok(())
}

fn process_increment_counter_signed(accounts: &[AccountInfo]) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;

    if !payer_info.is_signer {
        msg!("Payer is not signer");
        return Err(ProgramError::InvalidAccountData);
    }

    let (counter_pda, _) = Counter::pda(payer_info.key);
    assert_keys_equal(counter_pda_info.key, &counter_pda, || {
        format!(
            "Invalid counter PDA {}, should be {}",
            counter_pda_info.key, counter_pda
        )
    })?;
    if !counter_pda_info.is_writable {
        msg!("Counter PDA is not writable");
        return Err(ProgramError::InvalidAccountData);
    }
    if !counter_pda_info.is_signer {
        msg!("Counter PDA is not signer");
        return Err(ProgramError::InvalidAccountData);
    }

    let mut counter = Counter::try_from_slice(&counter_pda_info.data.borrow())?;

    counter.count += 1;

    let size = counter_pda_info.data_len();
    let counter_data = to_vec(&counter)?;
    counter_pda_info.data.borrow_mut()[..size].copy_from_slice(&counter_data);

    Ok(())
}

fn process_increment_counter_error() -> ProgramResult {
    Err(ProgramError::Custom(0))
}

fn process_delegate(
    accounts: &[AccountInfo],
    args: &DelegateArgs,
) -> ProgramResult {
    msg!("Delegate");
    let [payer, delegate_account_pda, owner_program, buffer, delegation_record, delegation_metadata, delegation_program, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let seeds_no_bump = Counter::seeds(payer.key);

    delegate_account(
        DelegateAccounts {
            payer,
            pda: delegate_account_pda,
            buffer,
            delegation_record,
            delegation_metadata,
            owner_program,
            delegation_program,
            system_program,
        },
        &seeds_no_bump,
        DelegateConfig {
            commit_frequency_ms: args.commit_frequency_ms,
            validator: None,
        },
    )?;

    Ok(())
}

fn process_undelegate_request(
    accounts: &[AccountInfo],
    seeds_data: &[u8],
) -> ProgramResult {
    msg!("Undelegate");
    let accounts_iter = &mut accounts.iter();
    let delegated_account = next_account_info(accounts_iter)?;
    let buffer = next_account_info(accounts_iter)?;
    let payer = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;
    let account_seeds =
        <Vec<Vec<u8>>>::try_from_slice(seeds_data).map_err(|err| {
            msg!("ERROR: failed to parse account seeds {:?}", err);
            ProgramError::InvalidArgument
        })?;

    undelegate_account(
        delegated_account,
        &crate::id(),
        buffer,
        payer,
        system_program,
        account_seeds,
    )?;
    Ok(())
}
