use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::cpi::{
    DelegateAccounts, DelegateConfig, UndelegateAccounts,
};
use ephemeral_rollups_sdk::ephem::{
    commit_accounts, commit_and_undelegate_accounts,
};
use ephemeral_rollups_sdk::{
    consts::EXTERNAL_UNDELEGATE_DISCRIMINATOR,
    cpi::{delegate_account, undelegate_account},
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::{self, ProgramResult},
    msg,
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
pub mod magicblock_program;
mod utils;

declare_id!("9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct DelegateCpiArgs {
    valid_until: i64,
    commit_frequency_ms: u32,
    player: Pubkey,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct ScheduleCommitCpiArgs {
    /// Pubkeys of players from which PDAs were derived
    pub players: Vec<Pubkey>,
    /// If true, the accounts will be modified after the commit
    pub modify_accounts: bool,
    /// If true, the accounts will be undelegated after the commit
    pub undelegate: bool,
    /// If true, also commit the payer account
    pub commit_payer: bool,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum ScheduleCommitInstruction {
    Init,

    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting delegation
    /// - **1.**   `[WRITE]`         Account for which delegation is requested
    /// - **2.**   `[]`              Delegate account owner program
    /// - **3.**   `[WRITE]`         Buffer account
    /// - **4.**   `[WRITE]`         Delegation record account
    /// - **5.**   `[WRITE]`         Delegation metadata account
    /// - **6.**   `[]`              Delegation program
    /// - **7.**   `[]`              System program
    DelegateCpi(DelegateCpiArgs),

    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
    /// - **1**    `[]`              MagicContext (used to record scheduled commit)
    /// - **2**    `[]`              MagicBlock Program (used to schedule commit)
    /// - **3..n** `[]`              PDA accounts to be committed
    ScheduleCommitCpi(ScheduleCommitCpiArgs),

    /// Same instruction input like [ScheduleCommitInstruction::ScheduleCommitCpi].
    /// Behavior differs that it will modify the accounts after it
    /// requested commit + undelegation.
    ///
    /// # Account references:
    /// - **0.**   `[WRITE]`         Delegated account
    /// - **1.**   `[]`              Delegation program
    /// - **2.**   `[WRITE]`         Buffer account
    /// - **3.**   `[WRITE]`         Payer
    /// - **4.**   `[]`              System program
    ScheduleCommitAndUndelegateCpiModAfter(Vec<Pubkey>),

    /// Increases the count of a PDA of this program by one.
    /// This instruction can only run on the ephemeral after the account was
    /// delegated or on chain while it is undelegated.
    /// # Account references:
    /// - **0.** `[WRITE]` Account to increase count
    IncreaseCount,
    // This is invoked by the delegation program when we request to undelegate
    // accounts.
    // # Account references:
    // - **0.** `[WRITE]` Account to be undelegated
    // - **1.** `[WRITE]` Buffer account
    // - **2.** `[WRITE]` Payer
    // - **3.** `[]` System program
    //
    // It is not part of this enum as it has a custom discriminator
    // Undelegate,
}

pub fn process_instruction<'a>(
    program_id: &'a Pubkey,
    accounts: &'a [AccountInfo<'a>],
    instruction_data: &[u8],
) -> ProgramResult {
    // Undelegate Instruction
    if instruction_data.len() >= EXTERNAL_UNDELEGATE_DISCRIMINATOR.len() {
        let (disc, seeds_data) =
            instruction_data.split_at(EXTERNAL_UNDELEGATE_DISCRIMINATOR.len());

        if disc == EXTERNAL_UNDELEGATE_DISCRIMINATOR {
            return process_undelegate_request(accounts, seeds_data);
        }
    }

    // Other instructions
    let ix = ScheduleCommitInstruction::try_from_slice(instruction_data)
        .map_err(|err| {
            msg!("ERROR: failed to parse instruction data {:?}", err);
            ProgramError::InvalidArgument
        })?;
    use ScheduleCommitInstruction::*;
    match ix {
        Init => process_init(program_id, accounts),
        DelegateCpi(args) => process_delegate_cpi(accounts, args),
        ScheduleCommitCpi(args) => process_schedulecommit_cpi(
            accounts,
            &args.players,
            ProcessSchedulecommitCpiArgs {
                modify_accounts: args.modify_accounts,
                undelegate: args.undelegate,
                commit_payer: args.commit_payer,
            },
        ),
        ScheduleCommitAndUndelegateCpiModAfter(players) => {
            process_schedulecommit_and_undelegation_cpi_with_mod_after(
                accounts, &players,
            )
        }
        IncreaseCount => process_increase_count(accounts),
    }
}

// -----------------
// Init
// -----------------
#[derive(BorshSerialize, BorshDeserialize, Debug, PartialEq, Eq, Clone)]
pub struct MainAccount {
    pub player: Pubkey,
    pub count: u64,
}

impl MainAccount {
    pub const SIZE: usize = std::mem::size_of::<Self>();

    pub fn try_decode(data: &[u8]) -> std::io::Result<Self> {
        Self::try_from_slice(data)
    }
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
pub fn process_delegate_cpi(
    accounts: &[AccountInfo],
    args: DelegateCpiArgs,
) -> Result<(), ProgramError> {
    msg!("Processing delegate_cpi instruction");

    let [payer, delegate_account_pda, owner_program, buffer, delegation_record, delegation_metadata, delegation_program, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let seeds_no_bump = pda_seeds(&args.player);

    msg!("seeds:  {:?}", seeds_no_bump);

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
            ..DelegateConfig::default()
        },
    )?;

    Ok(())
}

pub struct ProcessSchedulecommitCpiArgs {
    pub modify_accounts: bool,
    pub undelegate: bool,
    pub commit_payer: bool,
}

// -----------------
// Schedule Commit
// -----------------
pub fn process_schedulecommit_cpi(
    accounts: &[AccountInfo],
    player_pubkeys: &[Pubkey],
    args: ProcessSchedulecommitCpiArgs,
) -> Result<(), ProgramError> {
    msg!("Processing schedulecommit_cpi instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let magic_context = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;
    let mut remaining = vec![];
    for info in accounts_iter.by_ref() {
        remaining.push(info.clone());
    }

    if remaining.len() != player_pubkeys.len() {
        msg!(
            "ERROR: player_pubkeys.len() != committes.len() | {} != {}",
            player_pubkeys.len(),
            remaining.len()
        );
        return Err(ProgramError::InvalidArgument);
    }

    if args.modify_accounts {
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
    let mut account_infos = vec![payer, magic_context];
    account_infos.extend(remaining.iter());

    let mut committees = remaining.iter().collect::<Vec<_>>();
    if args.commit_payer {
        msg!("Commiting payer");
        committees.push(payer);
    }

    msg!(
        "Committees are {:?}",
        committees.iter().map(|x| x.key).collect::<Vec<_>>()
    );

    if args.undelegate {
        commit_and_undelegate_accounts(
            payer,
            committees,
            magic_context,
            magic_program,
        )?;
    } else {
        commit_accounts(payer, committees, magic_context, magic_program)?;
    }

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
// process_schedulecommit_and_undelegation_cpi_with_mod_after
// -----------------
fn process_schedulecommit_and_undelegation_cpi_with_mod_after(
    accounts: &[AccountInfo],
    player_pubkeys: &[Pubkey],
) -> Result<(), ProgramError> {
    msg!("Processing schedulecommit_and_undelegation_cpi_with_mod_after instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let magic_context = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;
    let mut remaining = vec![];
    for info in accounts_iter.by_ref() {
        remaining.push(info.clone());
    }

    if remaining.len() != player_pubkeys.len() {
        msg!(
            "ERROR: player_pubkeys.len() != committes.len() | {} != {}",
            player_pubkeys.len(),
            remaining.len()
        );
        return Err(ProgramError::InvalidArgument);
    }

    // Request the PDA accounts to be committed and undelegated
    let mut account_infos = vec![payer, magic_context];
    account_infos.extend(remaining.iter());

    commit_and_undelegate_accounts(
        payer,
        remaining.iter().collect::<Vec<_>>(),
        magic_context,
        magic_program,
    )?;

    // Then try to modify them
    // This fails because the owner is already changed to the delegation program
    // as part of undelegating the accounts
    for committee in &remaining {
        // Increase count of the PDA account
        let main_account = {
            let main_account_data = committee.try_borrow_data()?;
            let mut main_account =
                MainAccount::try_from_slice(&main_account_data)?;
            main_account.count += 1;
            main_account
        };
        main_account
            .serialize(&mut &mut committee.try_borrow_mut_data()?.as_mut())?;
    }

    Ok(())
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
        UndelegateAccounts {
            delegated_account,
            owner_program: &crate::id(),
            buffer,
            payer,
            system_program,
        },
        account_seeds,
    )?;
    Ok(())
}
