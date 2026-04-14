use solana_program::pubkey::Pubkey;

pub const CRANK_SEED: &[u8] = b"crank-executor";
const CRANK_SIGNER_PDA: ([u8; 32], u8) =
    const_crypto::ed25519::derive_program_address(
        &[CRANK_SEED],
        crate::ID.as_array(),
    );
pub const CRANK_SIGNER: Pubkey = Pubkey::new_from_array(CRANK_SIGNER_PDA.0);
pub const CRANK_SIGNER_BUMP: u8 = CRANK_SIGNER_PDA.1;
