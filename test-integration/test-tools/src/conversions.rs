use magicblock_core::magic_program;
use solana_sdk::pubkey::Pubkey;

pub fn pubkey_from_magic_program(pubkey: magic_program::Pubkey) -> Pubkey {
    Pubkey::from(pubkey.to_bytes())
}
