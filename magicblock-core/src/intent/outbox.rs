use solana_pubkey::Pubkey;

pub const OUTBOX_INTENT_SEED: &[u8] = b"outbox-intent";

pub const OUTBOX_INTENT_DISCRIMINATOR: [u8; 8] = *b"obintent";

/// Derives the outbox intent PDA for a given intent ID.
/// Seeds: `["outbox-intent", intent_id.to_le_bytes()]`
pub fn outbox_intent_pda(id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[OUTBOX_INTENT_SEED, &id.to_le_bytes()],
        &magicblock_magic_program_api::ID,
    )
    .0
}
