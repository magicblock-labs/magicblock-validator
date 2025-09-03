use solana_program::pubkey::Pubkey;

const ANCHOR_SEED: &[u8] = b"anchor:idl";
const SHANK_SEED: &[u8] = b"shank:idl";

pub enum IdlType {
    Anchor,
    Shank,
}

pub fn derive_counter_pda(program_id: &Pubkey, payer: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"counter", payer.as_ref()], program_id)
}

pub fn shank_idl_seeds_with_bump<'a>(
    program_id: &'a Pubkey,
    bump: &'a [u8],
) -> [&'a [u8]; 3] {
    [program_id.as_ref(), SHANK_SEED, bump]
}

pub fn derive_shank_idl_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[program_id.as_ref(), SHANK_SEED], program_id)
}

pub fn anchor_idl_seeds_with_bump<'a>(
    program_id: &'a Pubkey,
    bump: &'a [u8],
) -> [&'a [u8]; 3] {
    [program_id.as_ref(), ANCHOR_SEED, bump]
}

pub fn derive_anchor_idl_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[program_id.as_ref(), ANCHOR_SEED],
        program_id,
    )
}
