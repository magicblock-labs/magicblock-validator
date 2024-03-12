pub mod sleipnir_instruction;
pub mod sleipnir_processor;

use solana_sdk::{pubkey::Pubkey, signature::Keypair, signer::Signer};

// NOTE: this may have to be moved into a core module to be more accessible
solana_sdk::declare_id!("Luzid11111111111111111111111111111111111111");

pub const SLEIPNIR_AUTHORITY_ID: &str =
    "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";
const SLEIPNIR_AUTHORITY_SECRET: [u8; 64] = [
    216, 106, 211, 194, 246, 48, 44, 50, 135, 120, 121, 191, 235, 105, 83, 179,
    128, 200, 94, 89, 82, 215, 202, 178, 160, 36, 253, 217, 245, 42, 92, 157,
    5, 25, 245, 9, 49, 221, 122, 198, 124, 24, 136, 45, 57, 46, 55, 104, 42,
    68, 25, 104, 203, 22, 17, 45, 4, 77, 169, 95, 36, 9, 16, 33,
];

pub fn sleipnir_authority() -> Keypair {
    Keypair::from_bytes(&SLEIPNIR_AUTHORITY_SECRET).unwrap()
}

pub fn sleipnir_authority_id() -> Pubkey {
    sleipnir_authority().pubkey()
}
