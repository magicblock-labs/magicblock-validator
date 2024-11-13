use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::pubkey::Pubkey;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct FlexiCounter {
    pub count: u64,
    pub updates: u64,
    pub label: String,
}

const FLEXI_COUNTER_SEED: &[u8] = b"flexi_counter";

impl FlexiCounter {
    pub fn new(label: String) -> Self {
        Self {
            count: 0,
            updates: 0,
            label,
        }
    }
    pub fn seeds(payer: &Pubkey) -> [&[u8]; 3] {
        [crate::ID.as_ref(), FLEXI_COUNTER_SEED, payer.as_ref()]
    }

    pub fn seeds_with_bump<'a>(
        payer: &'a Pubkey,
        bump: &'a [u8],
    ) -> [&'a [u8]; 4] {
        [crate::ID.as_ref(), FLEXI_COUNTER_SEED, payer.as_ref(), bump]
    }

    pub fn pda(payer: &Pubkey) -> (Pubkey, u8) {
        let seeds = Self::seeds(payer);
        Pubkey::find_program_address(&seeds, &crate::id())
    }

    pub fn try_decode(data: &[u8]) -> std::io::Result<Self> {
        Self::try_from_slice(data)
    }
}
