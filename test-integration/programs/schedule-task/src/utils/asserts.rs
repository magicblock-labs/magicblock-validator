use solana_program::{
    entrypoint::ProgramResult, msg, program_error::ProgramError, pubkey::Pubkey,
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
