//! Test program for scheduled tasks that sign with their crank signer.
//!
//! It behaves like a program that trusts the crank signer: it fails unless the
//! crank signer of `authority` actually signed, then counts the execution.

use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    msg,
    program_error::ProgramError,
    pubkey,
    pubkey::Pubkey,
};

solana_program::declare_id!("ExcVtCcYhG47AXA6QygB3KesUkyN4XfHrWrh5QyRYGig");

/// Crank program that signs for crank signer PDAs.
pub const CRANK_PROGRAM_ID: Pubkey = pubkey!("Crank11111111111111111111111111111111111111");
/// Crank signer PDA seed: `["crank-executor", authority]`.
pub const CRANK_SEED: &[u8] = b"crank-executor";

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process);

/// Derives the crank signer PDA of `authority`.
pub fn crank_signer(authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[CRANK_SEED, authority.as_ref()], &CRANK_PROGRAM_ID).0
}

/// Builds the checked instruction.
///
/// # Account references
/// | Index | Account | Access |
/// | --- | --- | --- |
/// | `0` | Crank signer PDA of `authority`. | SIGNER |
/// | `1` | Task authority. | - |
/// | `2` | Counter owned by this program; first 8 bytes are a `u64` LE count. | WRITE |
pub fn check_instruction(authority: Pubkey, counter: Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        ID,
        &[],
        vec![
            AccountMeta::new_readonly(crank_signer(&authority), true),
            AccountMeta::new_readonly(authority, false),
            AccountMeta::new(counter, false),
        ],
    )
}

/// Fails unless account `0` signed and is the crank signer of account `1`,
/// then increments the counter in account `2`.
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo<'_>], _data: &[u8]) -> ProgramResult {
    let [signer, authority, counter, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !signer.is_signer {
        msg!("crank signer {} did not sign", signer.key);
        return Err(ProgramError::MissingRequiredSignature);
    }
    if signer.key != &crank_signer(authority.key) {
        msg!(
            "{} is not the crank signer of {}",
            signer.key,
            authority.key
        );
        return Err(ProgramError::InvalidSeeds);
    }
    if counter.owner != program_id || !counter.is_writable {
        return Err(ProgramError::IllegalOwner);
    }
    let mut data = counter.try_borrow_mut_data()?;
    let bytes: &mut [u8; 8] = data
        .get_mut(..8)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(ProgramError::AccountDataTooSmall)?;
    let count = u64::from_le_bytes(*bytes)
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    bytes.copy_from_slice(&count.to_le_bytes());
    msg!("crank signer {} verified, count {}", signer.key, count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crank_signer_matches_the_magic_program_api() {
        let authority = Pubkey::new_unique();
        let expected = magic_program_api::pda::crank_signer_pda(&authority.to_bytes().into());
        assert_eq!(crank_signer(&authority).to_bytes(), expected.to_bytes());
    }
}
