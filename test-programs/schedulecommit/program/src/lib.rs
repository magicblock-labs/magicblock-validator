use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::{self, ProgramResult},
    instruction::{AccountMeta, Instruction},
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    system_program,
};

use crate::{
    api::{pda_and_bump, pda_seeds_with_bump},
    utils::{
        allocate_account_and_assign_owner, assert_is_signer, assert_keys_equal,
        AllocateAndAssignAccountArgs,
    },
};
pub mod api;
mod utils;

declare_id!("9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction<'a>(
    program_id: &'a Pubkey,
    accounts: &'a [AccountInfo<'a>],
    instruction_data: &[u8],
) -> ProgramResult {
    let (instruction_discriminant, instruction_data_inner) =
        instruction_data.split_at(1);
    match instruction_discriminant[0] {
        0 => {
            process_init(program_id, accounts)?;
        }
        1 => {
            // # Account references
            // - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
            // - **1.**   `[SIGNER]`        The program owning the accounts to be committed
            // - **2.**   `[WRITE]`         Validator authority to which we escrow tx cost
            // - **3**    `[]`              MagicBlock Program (used to schedule commit)
            // - **4**    `[]`              System Program to support PDA signing
            // - **5..n** `[]`              Accounts to be committed
            process_schedulecommit_cpi(accounts, instruction_data_inner)?;
        }
        _ => {
            msg!("Error: unknown instruction")
        }
    }
    Ok(())
}

// -----------------
// Init
// -----------------
#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct MainAccount {
    pub player: Pubkey,
}

impl MainAccount {
    pub const SIZE: usize = std::mem::size_of::<Self>();
}

fn process_init<'a>(
    program_id: &'a Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> entrypoint::ProgramResult {
    msg!("Init account");
    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let pda_info = next_account_info(account_info_iter)?;

    assert_is_signer(payer_info, "payer")?;

    let (pda, bump) = pda_and_bump(payer_info.key);
    let bump_arr = [bump];
    let seeds = pda_seeds_with_bump(payer_info.key, &bump_arr);
    assert_keys_equal(pda_info.key, &pda, || {
        format!(
            "PDA for the account ('{}') and for payer ('{}') is incorrect",
            pda_info.key, payer_info.key
        )
    })?;
    allocate_account_and_assign_owner(AllocateAndAssignAccountArgs {
        payer_info,
        account_info: pda_info,
        owner: program_id,
        signer_seeds: &seeds,
        size: MainAccount::SIZE,
    })?;

    let account = MainAccount {
        player: *payer_info.key,
    };

    account.serialize(&mut &mut pda_info.try_borrow_mut_data()?.as_mut())?;

    Ok(())
}

pub fn process_schedulecommit_cpi(
    accounts: &[AccountInfo],
    _instruction_data: &[u8],
) -> Result<(), ProgramError> {
    msg!("Processing schedulecommit_cpi instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let owning_program = next_account_info(accounts_iter)?;
    let validator_auth = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;
    let remaining = accounts_iter.as_slice();
    let remaining_keys =
        remaining.iter().map(|a| *a.key).collect::<Vec<Pubkey>>();

    // THIS only works for one payer
    let (_, bump) = pda_and_bump(payer.key);
    let bump_arr = [bump];
    let signer_seeds = pda_seeds_with_bump(payer.key, &bump_arr);

    let ix = create_schedule_commit_ix(
        *payer.key,
        *owning_program.key,
        *validator_auth.key,
        *magic_program.key,
        &remaining_keys,
    );

    let mut account_infos = vec![
        payer.clone(),
        magic_program.clone(),
        validator_auth.clone(),
        system_program.clone(),
    ];
    account_infos.extend(remaining.iter().cloned());
    invoke_signed(&ix, &account_infos, &[&signer_seeds])?;

    Ok(())
}

/// # Account references
/// - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
/// - **1.**   `[SIGNER]`        The program owning the accounts to be committed
/// - **2.**   `[WRITE]`         Validator authority to which we escrow tx cost
/// - **3..n** `[]`              Accounts to be committed
fn create_schedule_commit_ix(
    payer: Pubkey,
    program_id: Pubkey,
    validator_id: Pubkey,
    magic_program_id: Pubkey,
    committees: &[Pubkey],
) -> Instruction {
    let instruction_data = vec![1, 0, 0, 0];
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(program_id, false),
        AccountMeta::new(validator_id, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    for committee in committees {
        account_metas.push(AccountMeta::new_readonly(*committee, false));
    }
    Instruction::new_with_bytes(
        magic_program_id,
        &instruction_data,
        account_metas,
    )
}
