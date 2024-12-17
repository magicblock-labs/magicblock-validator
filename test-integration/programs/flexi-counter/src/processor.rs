use borsh::{to_vec, BorshDeserialize};
use ephemeral_rollups_sdk::{
    consts::EXTERNAL_UNDELEGATE_DISCRIMINATOR,
    cpi::{delegate_account, undelegate_account},
    ephem::{commit_accounts, commit_and_undelegate_accounts},
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

use crate::{
    instruction::{DelegateArgs, FlexiCounterInstruction},
    state::FlexiCounter,
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

    let ix = FlexiCounterInstruction::try_from_slice(instruction_data)?;
    msg!("Processing instruction {:?}", ix);
    use FlexiCounterInstruction::*;
    match ix {
        Init { label, bump } => process_init(program_id, accounts, label, bump),
        Add { count } => process_add(accounts, count),
        Mul { multiplier } => process_mul(accounts, multiplier),
        Delegate(args) => process_delegate(accounts, &args),
        AddAndScheduleCommit { count, undelegate } => {
            process_add_and_schedule_commit(accounts, count, undelegate)
        }
        AddCounter => process_add_counter(accounts),
    }?;
    Ok(())
}

fn process_init(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    label: String,
    bump: u8,
) -> ProgramResult {
    msg!("Init {}", label);

    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;

    let (counter_pda, _) = FlexiCounter::pda(payer_info.key);
    assert_keys_equal(counter_pda_info.key, &counter_pda, || {
        format!(
            "Invalid Counter PDA {}, should be {}",
            counter_pda_info.key, counter_pda
        )
    })?;

    let bump = &[bump];
    let seeds = FlexiCounter::seeds_with_bump(payer_info.key, bump);

    let counter = FlexiCounter::new(label);

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

fn process_add(accounts: &[AccountInfo], count: u8) -> ProgramResult {
    msg!("Add {}", count);

    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;

    add(payer_info, counter_pda_info, count)
}

fn add(
    payer_info: &AccountInfo,
    counter_pda_info: &AccountInfo,
    count: u8,
) -> ProgramResult {
    let (counter_pda, _) = FlexiCounter::pda(payer_info.key);
    assert_keys_equal(counter_pda_info.key, &counter_pda, || {
        format!(
            "Invalid Counter PDA {}, should be {}",
            counter_pda_info.key, counter_pda
        )
    })?;

    let mut counter =
        FlexiCounter::try_from_slice(&counter_pda_info.data.borrow())?;

    counter.count += count as u64;
    counter.updates += 1;

    let size = counter_pda_info.data_len();
    let counter_data = to_vec(&counter)?;
    counter_pda_info.data.borrow_mut()[..size].copy_from_slice(&counter_data);

    Ok(())
}

fn process_mul(accounts: &[AccountInfo], multiplier: u8) -> ProgramResult {
    msg!("Mul {}", multiplier);

    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;

    let (counter_pda, _) = FlexiCounter::pda(payer_info.key);
    assert_keys_equal(counter_pda_info.key, &counter_pda, || {
        format!(
            "Invalid Counter PDA {}, should be {}",
            counter_pda_info.key, counter_pda
        )
    })?;

    let mut counter =
        FlexiCounter::try_from_slice(&counter_pda_info.data.borrow())?;

    counter.count *= multiplier as u64;
    counter.updates += 1;

    let size = counter_pda_info.data_len();
    let counter_data = to_vec(&counter)?;
    counter_pda_info.data.borrow_mut()[..size].copy_from_slice(&counter_data);

    Ok(())
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

    let seeds_no_bump = FlexiCounter::seeds(payer.key);

    delegate_account(
        payer,
        delegate_account_pda,
        owner_program,
        buffer,
        delegation_record,
        delegation_metadata,
        delegation_program,
        system_program,
        &seeds_no_bump,
        args.valid_until,
        args.commit_frequency_ms,
    )?;
    Ok(())
}

fn process_add_and_schedule_commit(
    accounts: &[AccountInfo],
    count: u8,
    undelegate: bool,
) -> ProgramResult {
    msg!(
        "Add {} and schedule commit undelegate: {}",
        count,
        undelegate
    );

    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let counter_pda_info = next_account_info(account_info_iter)?;
    let magic_context_info = next_account_info(account_info_iter)?;
    let magic_program_info = next_account_info(account_info_iter)?;

    // Perform the add operation
    add(payer_info, counter_pda_info, count)?;

    // Request the PDA counter account to be committed
    if undelegate {
        commit_and_undelegate_accounts(
            payer_info,
            vec![counter_pda_info],
            magic_context_info,
            magic_program_info,
        )?;
    } else {
        commit_accounts(
            payer_info,
            vec![counter_pda_info],
            magic_context_info,
            magic_program_info,
        )?;
    }
    Ok(())
}

fn process_add_counter(accounts: &[AccountInfo]) -> ProgramResult {
    msg!("AddCounter");

    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let target_pda_info = next_account_info(account_info_iter)?;
    let source_pda_info = next_account_info(account_info_iter)?;
    msg!("{} += {}", target_pda_info.key, source_pda_info.key);

    let (target_pda, _) = FlexiCounter::pda(payer_info.key);
    assert_keys_equal(target_pda_info.key, &target_pda, || {
        format!(
            "Invalid target Counter PDA {}, should be {}",
            target_pda_info.key, target_pda
        )
    })?;

    let source_counter =
        FlexiCounter::try_from_slice(&source_pda_info.data.borrow())?;
    let count = source_counter.count as u8;

    add(payer_info, target_pda_info, count)
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
