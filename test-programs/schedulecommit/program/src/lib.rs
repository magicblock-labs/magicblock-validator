use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::{self, ProgramResult},
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::{
    api::{pda_and_bump, pda_seeds_vec_with_bump, pda_seeds_with_bump},
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
    instruction_data: &[u8],
) -> Result<(), ProgramError> {
    msg!("Processing schedulecommit_cpi instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let owning_program = next_account_info(accounts_iter)?;
    let validator_auth = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;
    let mut remaining = vec![];
    for info in accounts_iter.by_ref() {
        let mut x = info.clone();
        x.is_signer = true;
        remaining.push(x);
    }

    let args = instruction_data.chunks(32).collect::<Vec<_>>();
    let player_pubkeys = args
        .into_iter()
        .map(Pubkey::try_from)
        .collect::<Result<Vec<Pubkey>, _>>()
        .map_err(|err| {
            msg!("ERROR: failed to parse player pubkey {:?}", err);
            ProgramError::InvalidArgument
        })?;

    if remaining.len() != player_pubkeys.len() {
        msg!(
            "ERROR: player_pubkeys.len() != committes.len() | {} != {}",
            player_pubkeys.len(),
            remaining.len()
        );
        return Err(ProgramError::InvalidArgument);
    }

    let mut player_bumps = vec![];
    for (player, committee) in player_pubkeys.iter().zip(remaining.iter()) {
        let (pda, bump) = pda_and_bump(player);
        if &pda != committee.key {
            msg!(
                "ERROR: pda(player) != committee PDA | '{}' != '{}'",
                player,
                committee.key
            );
            return Err(ProgramError::InvalidArgument);
        }
        player_bumps.push((player, bump));
    }

    let mut account_infos =
        vec![payer, owning_program, validator_auth, system_program];
    account_infos.extend(remaining.iter());

    msg!(
        "Committees are {:?}",
        remaining.iter().map(|x| x.key).collect::<Vec<_>>()
    );
    let ix = create_schedule_commit_ix(*magic_program.key, &account_infos);

    let seeds = player_bumps
        .into_iter()
        .map(|(x, y)| pda_seeds_vec_with_bump(*x, y))
        .collect::<Vec<_>>();
    let seeds = seeds
        .iter()
        .map(|xs| xs.iter().map(|x| x.as_slice()).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let seeds = seeds.iter().map(|x| x.as_slice()).collect::<Vec<_>>();

    invoke_signed(
        &ix,
        &account_infos.into_iter().cloned().collect::<Vec<_>>(),
        &seeds,
    )?;

    Ok(())
}

fn create_schedule_commit_ix(
    magic_program_key: Pubkey,
    account_infos: &[&AccountInfo],
) -> Instruction {
    let instruction_data = vec![1, 0, 0, 0];
    let account_metas = account_infos
        .iter()
        .map(|x| AccountMeta {
            pubkey: *x.key,
            is_signer: x.is_signer,
            is_writable: x.is_writable,
        })
        .collect::<Vec<AccountMeta>>();
    Instruction::new_with_bytes(
        magic_program_key,
        &instruction_data,
        account_metas,
    )
}
