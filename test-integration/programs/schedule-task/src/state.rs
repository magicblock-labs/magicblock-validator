use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::pubkey::Pubkey;

#[derive(
    BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq, Default,
)]
pub struct Counter {
    pub count: u64,
}

const COUNTER_SEED: &[u8] = b"counter";

impl Counter {
    pub fn seeds(payer: &Pubkey) -> [&[u8]; 3] {
        [crate::ID.as_ref(), COUNTER_SEED, payer.as_ref()]
    }

    pub fn seeds_with_bump<'a>(
        payer: &'a Pubkey,
        bump: &'a [u8],
    ) -> [&'a [u8]; 4] {
        [crate::ID.as_ref(), COUNTER_SEED, payer.as_ref(), bump]
    }

    pub fn pda(payer: &Pubkey) -> (Pubkey, u8) {
        let seeds = Self::seeds(payer);
        Pubkey::find_program_address(&seeds, &crate::id())
    }

    pub fn pda_with_bump(payer: &Pubkey, bump: u8) -> (Pubkey, u8) {
        let bumps = &[bump];
        let seeds = Self::seeds_with_bump(payer, bumps);
        Pubkey::find_program_address(&seeds, &crate::id())
    }

    pub fn try_decode(data: &[u8]) -> std::io::Result<Self> {
        Self::try_from_slice(data)
    }
}
