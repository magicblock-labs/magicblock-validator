use magicblock_committor_interface::{error::CommittorError, id};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    pubkey::Pubkey,
};

/// Validates the supplied account against the interface's canonical PDA seeds.
pub(crate) fn verify_pda(account: &AccountInfo<'_>, seeds: &[&[u8]], label: &str) -> ProgramResult {
    let expected = Pubkey::create_program_address(seeds, &id())
        .map_err(CommittorError::from)
        .inspect_err(|err| msg!("ERR: {}", err))?;
    if account.key != &expected {
        msg!(
            "Err: Provided {} PDA does not match derived key '{}'",
            label,
            expected
        );
        msg!("Err: provided {} expected {}", account.key, expected);
        Err(ProgramError::Custom(1))
    } else {
        Ok(())
    }
}

pub(crate) fn assert_account_unallocated(
    account: &AccountInfo<'_>,
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

pub(crate) fn assert_is_signer(account: &AccountInfo<'_>, account_label: &str) -> ProgramResult {
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

pub(crate) fn assert_program_id(program_id: &Pubkey) -> ProgramResult {
    if program_id != &id() {
        msg!(
            "Err: invalid program id, expected: {}, got: {}",
            id(),
            program_id
        );
        Err(ProgramError::IncorrectProgramId)
    } else {
        Ok(())
    }
}
