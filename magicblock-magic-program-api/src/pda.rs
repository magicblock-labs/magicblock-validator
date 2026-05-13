use solana_program::pubkey::Pubkey;

pub const CRANK_SEED: &[u8] = b"crank-executor";
pub fn crank_signer_pda(authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[CRANK_SEED, authority.as_ref()],
        &crate::CRANK_PROGRAM_ID,
    )
    .0
}

/// Callback signer PDA info
pub const CALLBACK_SEED: &[u8] = b"callback-executor";
const CALLBACK_SIGNER_PDA: ([u8; 32], u8) =
    const_crypto::ed25519::derive_program_address(
        &[CALLBACK_SEED],
        crate::CALLBACK_PROGRAM_ID.as_array(),
    );
pub const CALLBACK_SIGNER: Pubkey =
    Pubkey::new_from_array(CALLBACK_SIGNER_PDA.0);
pub const CALLBACK_SIGNER_BUMP: u8 = CALLBACK_SIGNER_PDA.1;
