use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::{
    consts::EXTERNAL_UNDELEGATE_DISCRIMINATOR,
    cpi::{delegate_account, undelegate_account},
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::{self, ProgramResult},
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::{
    api::{pda_and_bump, pda_seeds, pda_seeds_with_bump},
    utils::{
        allocate_account_and_assign_owner, assert_is_signer, assert_keys_equal,
        AllocateAndAssignAccountArgs,
    },
};
pub mod api;
pub mod sleipnir_program;
mod utils;

declare_id!("9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub const INIT_IX: u8 = 0;
pub const DELEGATE_CPI_IX: u8 = 1;
pub const SCHEDULECOMMIT_CPI_IX: u8 = 2;
pub const SCHEDULECOMMIT_AND_UNDELEGATE_CPI_IX: u8 = 3;
pub const INCREASE_COUNT_IX: u8 = 4;

const UNDELEGATE_IX: u8 = EXTERNAL_UNDELEGATE_DISCRIMINATOR[0];

pub fn process_instruction<'a>(
    program_id: &'a Pubkey,
    accounts: &'a [AccountInfo<'a>],
    instruction_data: &[u8],
) -> ProgramResult {
    let (instruction_discriminant, instruction_data_inner) =
        instruction_data.split_at(1);
    match instruction_discriminant[0] {
        INIT_IX => {
            process_init(program_id, accounts)?;
        }
        DELEGATE_CPI_IX => {
            // # Account references
            // - **0.**   `[WRITE, SIGNER]` Payer requesting delegation
            // - **1.**   `[WRITE]`         Account for which delegation is requested
            // - **2.**   `[]`              Delegate account owner program
            // - **3.**   `[WRITE]`         Buffer account
            // - **4.**   `[WRITE]`         Delegation record account
            // - **5.**   `[WRITE]`         Delegation metadata account
            // - **6.**   `[]`              Delegation program
            // - **7.**   `[]`              System program
            //
            // # Instruction Args
            //
            //  #[derive(Debug, BorshSerialize, BorshDeserialize)]
            //  pub struct DelegateCpiArgs {
            //      pub valid_until: i64,
            //      pub commit_frequency_ms: u32,
            //      pub player: Pubkey,
            //  }
            process_delegate_cpi(accounts, instruction_data_inner)?
        }
        SCHEDULECOMMIT_CPI_IX => {
            // # Account references
            // - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
            // - **1**    `[]`              MagicBlock Program (used to schedule commit)
            // - **2..n** `[]`              PDA accounts to be committed
            //
            // # Instruction Args
            //
            // - **0..32**   Player 1 pubkey from which first PDA was derived
            // - **32..64**  Player 2 pubkey from which second PDA was derived
            // - **n..n+32** Player n pubkey from which n-th PDA was derived
            process_schedulecommit_cpi(
                accounts,
                instruction_data_inner,
                true,
                false,
            )?;
        }
        // # Account references:
        // - **0.**   `[WRITE]`         Delegated account
        // - **1.**   `[]`              Delegation program
        // - **2.**   `[WRITE]`         Buffer account
        // - **3.**   `[WRITE]`         Payer
        // - **4.**   `[]`              System program
        SCHEDULECOMMIT_AND_UNDELEGATE_CPI_IX => {
            // Same instruction input like [SCHEDULECOMMIT_CPI_IX].
            // Behavior differs that it will request undelegation of committed accounts.
            process_schedulecommit_cpi(
                accounts,
                instruction_data_inner,
                true,
                true,
            )?;
        }
        // Increases the count of a PDA of this program by one.
        // This instruction can only run on the ephemeral after the account was
        // delegated or on chain while it is undelegated.
        // # Account references:
        // - **0.** `[WRITE]` Account to increase count
        INCREASE_COUNT_IX => {
            process_increase_count(accounts)?;
        }
        // This is invoked by the delegation program when we request to undelegate
        // accounts.
        // # Account references:
        // - **0.** `[WRITE]` Account to be undelegated
        // - **1.** `[WRITE]` Buffer account
        // - **2.** `[WRITE]` Payer
        // - **3.** `[]` System program
        UNDELEGATE_IX => {
            let (disc, seeds_data) = instruction_data
                .split_at(EXTERNAL_UNDELEGATE_DISCRIMINATOR.len());
            if disc != EXTERNAL_UNDELEGATE_DISCRIMINATOR {
                msg!("Error: unknown instruction: [{:?}] (had assumed undelegate)", disc);
                msg!("Instruction data: {:?}", instruction_data);
                return Err(ProgramError::InvalidInstructionData);
            }

            process_undelegate_request(accounts, seeds_data)?;
        }
        discriminant => {
            msg!("Error: unknown instruction: [{}]", discriminant);
            msg!("Instruction data: {:?}", instruction_data);
            return Err(ProgramError::InvalidInstructionData);
        }
    }
    Ok(())
}

// -----------------
// Init
// -----------------
#[derive(BorshSerialize, BorshDeserialize, Debug, PartialEq, Eq)]
pub struct MainAccount {
    pub player: Pubkey,
    pub count: u64,
}

impl MainAccount {
    pub const SIZE: usize = std::mem::size_of::<Self>();
}

// -----------------
// Init
// -----------------
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
    let seeds_no_bump = pda_seeds(payer_info.key);
    msg!("payer:    {}", payer_info.key);
    msg!("pda:      {}", pda);
    msg!("seeds:    {:?}", seeds);
    msg!("seedsnb:  {:?}", seeds_no_bump);
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
        count: 0,
    };

    account.serialize(&mut &mut pda_info.try_borrow_mut_data()?.as_mut())?;

    Ok(())
}

// -----------------
// Delegate
// -----------------
#[derive(Debug, BorshSerialize, BorshDeserialize)]
pub struct DelegateCpiArgs {
    pub valid_until: i64,
    pub commit_frequency_ms: u32,
    pub player: Pubkey,
}

pub fn process_delegate_cpi(
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> Result<(), ProgramError> {
    msg!("Processing delegate_cpi instruction");

    let [payer, delegate_account_pda, owner_program, buffer, delegation_record, delegation_metadata, delegation_program, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let args =
        DelegateCpiArgs::try_from_slice(instruction_data).map_err(|err| {
            msg!("ERROR: failed to parse delegate account args {:?}", err);
            ProgramError::InvalidArgument
        })?;
    let seeds_no_bump = pda_seeds(&args.player);

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

// -----------------
// Schedule Commit
// -----------------
pub fn process_schedulecommit_cpi(
    accounts: &[AccountInfo],
    instruction_data: &[u8],
    modify_accounts: bool,
    undelegate: bool,
) -> Result<(), ProgramError> {
    msg!("Processing schedulecommit_cpi instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;
    let mut remaining = vec![];
    for info in accounts_iter.by_ref() {
        remaining.push(info.clone());
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

    if modify_accounts {
        for committee in &remaining {
            // Increase count of the PDA account
            let main_account = {
                let main_account_data = committee.try_borrow_data()?;
                let mut main_account =
                    MainAccount::try_from_slice(&main_account_data)?;
                main_account.count += 1;
                main_account
            };
            main_account.serialize(
                &mut &mut committee.try_borrow_mut_data()?.as_mut(),
            )?;
        }
    }

    // Then request the PDA accounts to be committed
    let mut account_infos = vec![payer];
    account_infos.extend(remaining.iter());

    // NOTE: logging this increases CPUs by 70K, so in order to show about how
    // many CPUs scheduling a commit actually takes we removed this log
    // msg!(
    //     "Committees are {:?}",
    //     remaining.iter().map(|x| x.key).collect::<Vec<_>>()
    // );
    let ix = create_schedule_commit_ix(
        *magic_program.key,
        &account_infos,
        undelegate,
    );

    invoke(&ix, &account_infos.into_iter().cloned().collect::<Vec<_>>())?;

    Ok(())
}

fn process_increase_count(accounts: &[AccountInfo]) -> ProgramResult {
    msg!("Processing increase_count instruction");
    // NOTE: we don't check if the player owning the PDA is signer here for simplicity
    let accounts_iter = &mut accounts.iter();
    let account = next_account_info(accounts_iter)?;
    let mut main_account = {
        let main_account_data = account.try_borrow_data()?;
        MainAccount::try_from_slice(&main_account_data)?
    };
    main_account.count += 1;
    main_account
        .serialize(&mut &mut account.try_borrow_mut_data()?.as_mut())?;
    Ok(())
}

// -----------------
// create_schedule_commit_ix
// -----------------
pub fn create_schedule_commit_ix(
    magic_program_key: Pubkey,
    account_infos: &[&AccountInfo],
    undelegate: bool,
) -> Instruction {
    let ix = if undelegate {
        sleipnir_program::SleipnirInstruction::ScheduleCommitAndUndelegate
    } else {
        sleipnir_program::SleipnirInstruction::ScheduleCommit
    };
    let instruction_data = ix.discriminant();
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

// -----------------
// Undelegate Request
// -----------------
fn process_undelegate_request(
    accounts: &[AccountInfo],
    seeds_data: &[u8],
) -> ProgramResult {
    msg!("Processing undelegate_request instruction");
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
