use magicblock_program::validator::validator_authority_id;
use solana_pubkey::Pubkey;

/// Seed for deriving buffer account PDA
const BUFFER_SEED: &[u8] = b"buffer";

/// Derives a deterministic buffer account pubkey for program cloning.
/// Uses validator_authority as owner so it works for any loader type.
pub fn derive_buffer_pubkey(program_pubkey: &Pubkey) -> (Pubkey, u8) {
    let seeds: &[&[u8]] = &[BUFFER_SEED, program_pubkey.as_ref()];
    Pubkey::find_program_address(seeds, &validator_authority_id())
}
