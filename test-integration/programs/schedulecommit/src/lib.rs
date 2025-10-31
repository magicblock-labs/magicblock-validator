use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::{
    consts::EXTERNAL_UNDELEGATE_DISCRIMINATOR,
    cpi::{
        delegate_account, undelegate_account, DelegateAccounts, DelegateConfig,
    },
    ephem::{
        commit_accounts, commit_and_undelegate_accounts,
        commit_diff_and_undelegate_accounts,
    },
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::{self, ProgramResult},
    msg,
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
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
mod order_book;
mod utils;

use order_book::*;

pub use order_book::{BookUpdate, OrderBookOwned, OrderLevel};

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
pub struct DelegateOrderBookArgs {
    commit_frequency_ms: u32,
    book_manager: Pubkey,
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
    /// - **0.**   `[WRITE, SIGNER]` Payer funding the initialization
    /// - **1.**   `[SIGNER]`        Player requesting initialization
    /// - **2.**   `[WRITE]`         Account for which initialization is requested
    /// - **3.**   `[]`              System program
    Init,

    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting and funcding the delegation
    /// - **1.**   `[WRITE]`         Account for which delegation is requested
    /// - **2.**   `[]`              Delegate account owner program
    /// - **3.**   `[WRITE]`         Buffer account
    /// - **4.**   `[WRITE]`         Delegation record account
    /// - **5.**   `[WRITE]`         Delegation metadata account
    /// - **6.**   `[]`              Delegation program
    /// - **7.**   `[]`              System program
    DelegateCpi(DelegateCpiArgs),

    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer funding the commit
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
    /// - **0.** `[WRITE]` PDA Account to increase count of
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
    /// Initialize an OrderBook
    InitOrderBook,

    GrowOrderBook(u64), // additional_space

    /// Delegate order book to ER nodes
    DelegateOrderBook(DelegateOrderBookArgs),

    /// Update order book
    UpdateOrderBook(BookUpdate),

    /// ScheduleCommitDiffCpi
    ScheduleCommitForOrderBook,
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
        InitOrderBook => process_init_order_book(accounts),
        GrowOrderBook(additional_space) => {
            process_grow_order_book(accounts, additional_space)
        }
        DelegateOrderBook(args) => process_delegate_order_book(accounts, args),
        UpdateOrderBook(args) => process_update_order_book(accounts, args),
        ScheduleCommitForOrderBook => {
            process_schedulecommit_for_orderbook(accounts)
        }
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
    pub const SIZE: u64 = std::mem::size_of::<Self>() as u64;

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
    let player_info = next_account_info(account_info_iter)?;
    let pda_info = next_account_info(account_info_iter)?;

    assert_is_signer(player_info, "payer")?;

    let (pda, bump) = pda_and_bump(player_info.key);
    let bump_arr = [bump];
    let seeds = pda_seeds_with_bump(player_info.key, &bump_arr);
    let seeds_no_bump = pda_seeds(player_info.key);
    msg!(
        "payer:    {} | {} | {}",
        payer_info.key,
        payer_info.owner,
        payer_info.lamports()
    );
    msg!(
        "player:   {} | {} | {}",
        player_info.key,
        player_info.owner,
        player_info.lamports()
    );
    msg!("pda:      {} | {}", pda, pda_info.owner);
    msg!("seeds:    {:?}", seeds);
    msg!("seedsnb:  {:?}", seeds_no_bump);
    assert_keys_equal(pda_info.key, &pda, || {
        format!(
            "PDA for the account ('{}') and for payer ('{}') is incorrect",
            pda_info.key, player_info.key
        )
    })?;
    allocate_account_and_assign_owner(AllocateAndAssignAccountArgs {
        payer_info,
        account_info: pda_info,
        owner: program_id,
        signer_seeds: &seeds,
        size: MainAccount::SIZE,
    })?;

    msg!(
        "pda_info: {} | {} | {} | len: {}",
        pda_info.key,
        pda_info.owner,
        pda_info.lamports(),
        pda_info.data_len()
    );

    let account = MainAccount {
        player: *player_info.key,
        count: 0,
    };

    let mut acc_data = pda_info.try_borrow_mut_data()?;
    account.serialize(&mut &mut acc_data.as_mut())?;

    Ok(())
}

// -----------------
// InitOrderBook
// -----------------
fn process_init_order_book<'a>(
    accounts: &'a [AccountInfo<'a>],
) -> entrypoint::ProgramResult {
    msg!("Init OrderBook account");
    let [payer, book_manager, order_book, _system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_is_signer(payer, "payer")?;

    let (pda, bump) = Pubkey::find_program_address(
        &[b"order_book", book_manager.key.as_ref()],
        &crate::ID,
    );

    assert_keys_equal(order_book.key, &pda, || {
        format!(
            "PDA for the account ('{}') and for book_manager ('{}') is incorrect",
            order_book.key, book_manager.key
        )
    })?;

    allocate_account_and_assign_owner(AllocateAndAssignAccountArgs {
        payer_info: payer,
        account_info: order_book,
        owner: &crate::ID,
        signer_seeds: &[b"order_book", book_manager.key.as_ref(), &[bump]],
        size: 10 * 1024,
    })?;

    Ok(())
}

fn process_grow_order_book<'a>(
    accounts: &'a [AccountInfo<'a>],
    additional_space: u64,
) -> entrypoint::ProgramResult {
    msg!("Grow OrderBook account");
    let [payer, book_manager, order_book, system_program] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    assert_is_signer(payer, "payer")?;

    let (pda, _bump) = Pubkey::find_program_address(
        &[b"order_book", book_manager.key.as_ref()],
        &crate::ID,
    );

    assert_keys_equal(order_book.key, &pda, || {
        format!(
            "PDA for the account ('{}') and for book_manager ('{}') is incorrect",
            order_book.key, payer.key
        )
    })?;

    let new_size = order_book.data_len() + additional_space as usize;

    // Ideally, we should transfer some lamports from payer to order_book
    // so that realloc could use it

    let rent = Rent::get()?;
    let required = rent.minimum_balance(new_size);
    let current = order_book.lamports();
    if current < required {
        let diff = required - current;
        invoke(
            &system_instruction::transfer(payer.key, order_book.key, diff),
            &[payer.clone(), order_book.clone(), system_program.clone()],
        )?;
    }

    order_book.realloc(new_size, true)?;

    Ok(())
}

// -----------------
// Delegate OrderBook
// -----------------
pub fn process_delegate_order_book(
    accounts: &[AccountInfo],
    args: DelegateOrderBookArgs,
) -> Result<(), ProgramError> {
    msg!("Processing delegate_order_book instruction");

    let [payer, order_book, owner_program, buffer, delegation_record, delegation_metadata, delegation_program, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let seeds_no_bump = [b"order_book", args.book_manager.as_ref()];

    delegate_account(
        DelegateAccounts {
            payer,
            pda: order_book,
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

// -----------------
// UpdateOrderBook
// -----------------
fn process_update_order_book<'a>(
    accounts: &'a [AccountInfo<'a>],
    updates: BookUpdate,
) -> entrypoint::ProgramResult {
    msg!("Init account");
    let account_info_iter = &mut accounts.iter();
    let payer_info = next_account_info(account_info_iter)?;
    let order_book_account = next_account_info(account_info_iter)?;

    assert_is_signer(payer_info, "payer")?;

    let mut book_raw = order_book_account.try_borrow_mut_data()?;

    OrderBook::new(&mut book_raw).update_from(updates);

    Ok(())
}

// -----------------
// Schedule Commit
// -----------------
pub fn process_schedulecommit_for_orderbook(
    accounts: &[AccountInfo],
) -> Result<(), ProgramError> {
    msg!("Processing schedulecommit (for orderbook) instruction");

    let [payer, order_book_account, magic_context, magic_program] = accounts
    else {
        return Err(ProgramError::MissingRequiredSignature);
    };

    // IMPORTANT: it seems the scoping matters to avoid following error:
    //  -  TransactionError(InstructionError(0, AccountBorrowFailed))
    //  - Program 9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY failed: instruction tries to borrow reference for an account which is already borrowed
    {
        // let mut book_raw = order_book_account.try_borrow_mut_data()?;
        // let mut order_book = OrderBook::new(&mut book_raw);

        // order_book.add_bids(&[OrderLevel {
        //     price: 90000,
        //     size: 10,
        // }]);
        // order_book.add_asks(&[OrderLevel {
        //     price: 125000,
        //     size: 16,
        // }]);
    }

    commit_diff_and_undelegate_accounts(
        payer,
        vec![order_book_account],
        magic_context,
        magic_program,
    )?;

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
        commit_diff_and_undelegate_accounts(
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
    msg!("Counter account key {}", account.key);
    let mut main_account = {
        let main_account_data = account.try_borrow_data()?;
        MainAccount::try_from_slice(&main_account_data)?
    };
    msg!("Owner: {}", account.owner);
    msg!("Counter account {:#?}", main_account);
    main_account.count += 1;
    msg!("Increased count {:#?}", main_account);
    let mut mut_data = account.try_borrow_mut_data()?;
    let mut as_mut: &mut [u8] = mut_data.as_mut();
    msg!("Mutating buffer of len: {}", as_mut.len());
    main_account.serialize(&mut as_mut)?;
    msg!("Serialized counter");
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
        delegated_account,
        &id(),
        buffer,
        payer,
        system_program,
        account_seeds,
    )?;
    Ok(())
}
