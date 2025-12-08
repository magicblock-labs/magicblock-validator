use solana_pubkey::{pubkey, Pubkey};

// Shared program IDs and helper functions for SPL Token, Associated Token, and eATA programs.

// SPL Token Program ID (Tokenkeg...)
pub const SPL_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// Associated Token Account Program ID (ATokenG...)
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

// Enhanced ATA (eATA) Program ID (5iC4wK...)
pub const EATA_PROGRAM_ID: Pubkey =
    pubkey!("5iC4wKZizyxrKh271Xzx3W4Vn2xUyYvSGHeoB2mdw5HA");

// Derive the standard ATA address for a given wallet owner and mint.
pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

// Try to derive the ATA address returning both address and bump if derivation succeeds.
pub fn try_derive_ata_address_and_bump(
    owner: &Pubkey,
    mint: &Pubkey,
) -> Option<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

// Derive the eATA PDA for a given wallet owner and mint.
pub fn derive_eata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), mint.as_ref()],
        &EATA_PROGRAM_ID,
    )
    .0
}

// Try to derive the eATA PDA returning both address and bump if derivation succeeds.
pub fn try_derive_eata_address_and_bump(
    owner: &Pubkey,
    mint: &Pubkey,
) -> Option<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[owner.as_ref(), mint.as_ref()],
        &EATA_PROGRAM_ID,
    )
}
