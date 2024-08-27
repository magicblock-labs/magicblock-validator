use std::str::FromStr;

use lazy_static::lazy_static;
use solana_sdk::pubkey::Pubkey;

pub mod account_actions;
pub mod accounts;
#[cfg(not(test))]
pub(crate) mod instruction_context_frames;

// NOTE: there is no low level SDK currently that exposes the program address
//       we hardcode it here to avoid either having to pull in the delegation program
//       or a higher level SDK including procmacros for CPI, etc.
pub const DELEGATION_PROGRAM_ADDR: &str =
    "DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh";

lazy_static! {
    pub static ref DELEGATION_PROGRAM_ID: Pubkey =
        Pubkey::from_str(DELEGATION_PROGRAM_ADDR).unwrap();
}
