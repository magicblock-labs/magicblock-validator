use solana_pubkey::Pubkey;

pub(crate) mod account_actions;
pub(crate) mod accounts;
pub mod instruction_utils;

// NOTE: there is no low level SDK currently that exposes the program address
//       we hardcode it here to avoid either having to pull in the delegation program
//       or a higher level SDK including procmacros for CPI, etc.
pub(crate) const DELEGATION_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
