use solana_program::{
    account_info::{next_account_info, AccountInfo},
    clock::Clock,
    entrypoint::ProgramResult,
    epoch_schedule::EpochSchedule,
    instruction::get_processed_sibling_instruction,
    msg,
    pubkey::Pubkey,
    rent::Rent,
    sysvar::{
        instructions::{
            get_instruction_relative, load_current_index_checked, load_instruction_at_checked,
        },
        Sysvar,
    },
};

#[allow(deprecated)]
use solana_program::sysvar::{fees::Fees, recent_blockhashes::RecentBlockhashes};

solana_program::entrypoint!(process_instruction);

fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match instruction_data[0] {
        0 => process_sysvar_get(program_id, accounts),
        1 => process_sysvar_from_account(program_id, accounts),
        _ => {
            msg!("Instruction not supported");
            Ok(())
        }
    }
}

fn process_sysvar_get(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    msg!("Processing sysvar_get instruction");
    msg!("program_id: {}", program_id);
    msg!("accounts: {}", accounts.len());

    let clock: Clock = Clock::get().unwrap();
    msg!("{:?}", clock);
    let rent = Rent::get().unwrap();
    msg!("{:?}", rent);
    let epoch_schedule = EpochSchedule::get().unwrap();
    msg!("{:?}", epoch_schedule);

    Ok(())
}

fn process_sysvar_from_account(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    msg!("Processing sysvar_from_account instruction");
    msg!("program_id: {}", program_id);
    msg!("accounts: {}", accounts.len());

    let accounts_iter = &mut accounts.iter();
    let _payer = next_account_info(accounts_iter)?;
    let clock_account = next_account_info(accounts_iter)?;
    let rent_account = next_account_info(accounts_iter)?;
    let epoch_schedule_account = next_account_info(accounts_iter)?;
    let fees_account = next_account_info(accounts_iter)?;
    let recent_blockhashes_account = next_account_info(accounts_iter)?;
    let last_restart_slot_account = next_account_info(accounts_iter)?;
    let ix_introspections_account = next_account_info(accounts_iter)?;

    let clock = Clock::from_account_info(clock_account).unwrap();
    msg!("{:?}", clock);

    let rent = Rent::from_account_info(rent_account).unwrap();
    msg!("{:?}", rent);

    let epoch_schedule = EpochSchedule::from_account_info(epoch_schedule_account).unwrap();
    msg!("{:?}", epoch_schedule);

    #[allow(deprecated)]
    let fees = Fees::from_account_info(fees_account).unwrap();
    msg!("{:?}", fees);

    #[allow(deprecated)]
    let recent_blockhashes =
        RecentBlockhashes::from_account_info(recent_blockhashes_account).unwrap();
    msg!("{:?}", recent_blockhashes);

    // Showing here that we don't provide this yet since our feature set isn't enabling last_restart_slot
    // NOTE: data.len: 0
    msg!("{:?}", last_restart_slot_account);

    // -----------------
    // Instruction Inspection
    // -----------------
    let current_ix_idx = load_current_index_checked(ix_introspections_account)?;
    msg!("Instruction index: {}", current_ix_idx);

    let ix_before = get_instruction_relative(-1, ix_introspections_account)?;
    let ix_after = get_instruction_relative(1, ix_introspections_account)?;
    // let ix_sibling = get_processed_sibling_instruction(current_ix_idx as usize)
    // .expect("Should have sibling instruction");

    let ix_info = load_instruction_at_checked(current_ix_idx as usize, ix_introspections_account)?;
    msg!("Instruction info: {:?}", ix_info);
    msg!("Instruction before: {:?}", ix_before);
    msg!("Instruction after: {:?}", ix_after);
    // msg!("Instruction sibling: {:?}", ix_sibling);

    // assert_eq!(ix_sibling, ix_before);

    Ok(())
}
