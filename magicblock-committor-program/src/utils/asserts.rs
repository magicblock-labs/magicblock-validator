use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg,
    program_error::ProgramError, pubkey::Pubkey,
};

pub fn assert_keys_equal<F: FnOnce() -> String>(
    provided_key: &Pubkey,
    expected_key: &Pubkey,
    get_msg: F,
) -> ProgramResult {
    if provided_key.ne(expected_key) {
        msg!("Err: {}", get_msg());
        msg!("Err: provided {} expected {}", provided_key, expected_key);
        Err(ProgramError::Custom(1))
    } else {
        Ok(())
    }
}

pub fn assert_account_unallocated(
    account: &AccountInfo,
    account_label: &str,
) -> ProgramResult {
    if account.try_borrow_data()?.len() != 0 {
        msg!(
            "Err: account '{}' ({}) was already initialized",
            account_label,
            account.key
        );
        Err(ProgramError::AccountAlreadyInitialized)
    } else {
        Ok(())
    }
}

pub fn assert_is_signer(
    account: &AccountInfo,
    account_label: &str,
) -> ProgramResult {
    if !account.is_signer {
        msg!(
            "Err: account '{}' ({}) should be signer",
            account_label,
            account.key
        );
        Err(ProgramError::MissingRequiredSignature)
    } else {
        Ok(())
    }
}

pub fn assert_program_id(program_id: &Pubkey) -> ProgramResult {
    if program_id != &crate::id() {
        msg!("Err: invalid program id");
        Err(ProgramError::IncorrectProgramId)
    } else {
        Ok(())
    }
}
