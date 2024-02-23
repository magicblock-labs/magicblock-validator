use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint, entrypoint::ProgramResult,
    epoch_schedule::EpochSchedule, msg, pubkey::Pubkey, rent::Rent, sysvar::Sysvar,
};

entrypoint!(process_instruction);

fn process_instruction(
    _program_id: &Pubkey,
    _accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    match instruction_data[0] {
        0 => process_sysvar_get(_program_id, _accounts),
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
    msg!("{:#?}", clock);
    let rent = Rent::get().unwrap();
    msg!("{:#?}", rent);
    let epoch_schedule = EpochSchedule::get().unwrap();
    msg!("{:#?}", epoch_schedule);
    Ok(())
}
