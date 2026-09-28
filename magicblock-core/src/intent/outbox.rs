use solana_pubkey::Pubkey;

pub const OUTBOX_INTENT_SEED: &[u8] = b"outbox-intent";

pub const OUTBOX_INTENT_DISCRIMINATOR: [u8; 8] = *b"obintent";

/// Derives the outbox intent PDA for a given intent ID.
/// Seeds: `["outbox-intent", intent_id.to_le_bytes()]`
pub fn outbox_intent_pda(id: u64) -> Pubkey {
    outbox_intent_pda_with_bump(id).0
}

/// Same as [`outbox_intent_pda`], but also returns the canonical bump seed.
/// Store the bump alongside the account so later validations can use the
/// much cheaper [`verify_outbox_intent_pda`] instead of re-deriving via
/// `find_program_address`.
pub fn outbox_intent_pda_with_bump(id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[OUTBOX_INTENT_SEED, &id.to_le_bytes()],
        &magicblock_magic_program_api::ID,
    )
}

/// Cheaply confirms `candidate` is the outbox intent PDA for `id`, given its
/// previously-stored canonical `bump`. Uses `create_program_address` (a
/// single hash) instead of `find_program_address`'s bump search.
pub fn verify_outbox_intent_pda(id: u64, bump: u8, candidate: &Pubkey) -> bool {
    Pubkey::create_program_address(
        &[OUTBOX_INTENT_SEED, &id.to_le_bytes(), &[bump]],
        &magicblock_magic_program_api::ID,
    )
    .is_ok_and(|derived| derived == *candidate)
}
