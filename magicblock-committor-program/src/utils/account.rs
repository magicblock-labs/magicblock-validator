use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg,
    program_error::ProgramError,
};

pub fn close_and_refund_authority(
    authority: &AccountInfo,
    account: &AccountInfo,
) -> ProgramResult {
    // Realloc the account data to len 0 to avoid refunding attacks, i.e. keeping
    // the account around in an instruction that is appended as part of this
    // transaction
    // https://www.helius.dev/blog/a-hitchhikers-guide-to-solana-program-security
    account.realloc(0, false)?;

    // Transfer all lamports to authority
    **authority.lamports.borrow_mut() = authority
        .lamports()
        .checked_add(account.lamports())
        .ok_or_else(|| {
            msg!("Overflow when refunding authority");
            ProgramError::ArithmeticOverflow
        })?;
    **account.lamports.borrow_mut() = 0;

    Ok(())
}
