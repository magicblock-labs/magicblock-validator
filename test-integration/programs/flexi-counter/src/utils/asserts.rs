#![allow(unused)]
use solana_program::pubkey::Pubkey;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg,
    program_error::ProgramError,
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

pub fn assert_size(
    account: &AccountInfo,
    size: usize,
    account_label: &str,
) -> ProgramResult {
    let account_size = account.try_data_len()?;
    if account_size != size {
        msg!(
            "Err: account '{}' should have size {} but has {}",
            account_label,
            account_size,
            size
        );
        Err(ProgramError::InvalidAccountData)
    } else {
        Ok(())
    }
}
