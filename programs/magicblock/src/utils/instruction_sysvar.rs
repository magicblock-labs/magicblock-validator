use solana_account_info::AccountInfo;
use solana_instruction::{
    error::{
        InstructionError, INVALID_ARGUMENT, INVALID_INSTRUCTION_DATA,
        UNSUPPORTED_SYSVAR,
    },
    Instruction,
};
use solana_instructions_sysvar::{
    load_current_index_checked, load_instruction_at_checked,
};
use solana_pubkey::Pubkey;

pub(crate) fn load_current_index(
    data: &mut [u8],
) -> Result<usize, InstructionError> {
    if data.len() < 2 {
        return Err(InstructionError::AccountDataTooSmall);
    }
    with_instructions_account_info(data, |account_info| {
        load_current_index_checked(account_info)
    })
    .map(usize::from)
}

pub(crate) fn load_instruction_at(
    data: &mut [u8],
    index: usize,
) -> Result<Instruction, InstructionError> {
    with_instructions_account_info(data, |account_info| {
        load_instruction_at_checked(index, account_info)
    })
}

fn with_instructions_account_info<T, E>(
    data: &mut [u8],
    f: impl FnOnce(&AccountInfo<'_>) -> Result<T, E>,
) -> Result<T, InstructionError>
where
    E: Into<u64>,
{
    let key = solana_sdk_ids::sysvar::instructions::id();
    let owner = Pubkey::default();
    let mut lamports = 0;
    let account_info = AccountInfo::new(
        &key,
        false,
        false,
        &mut lamports,
        data,
        &owner,
        false,
    );

    f(&account_info).map_err(program_error_to_instruction_error)
}

fn program_error_to_instruction_error(
    error: impl Into<u64>,
) -> InstructionError {
    match error.into() {
        INVALID_ARGUMENT => InstructionError::InvalidArgument,
        INVALID_INSTRUCTION_DATA => InstructionError::InvalidInstructionData,
        UNSUPPORTED_SYSVAR => InstructionError::UnsupportedSysvar,
        _ => InstructionError::InvalidInstructionData,
    }
}
