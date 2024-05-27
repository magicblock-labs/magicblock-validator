pub mod commit_sender;
mod errors;
pub mod sleipnir_instruction;
pub mod sleipnir_processor;
mod validator;

pub use validator::*;

// NOTE: this may have to be moved into a core module to be more accessible
solana_sdk::declare_id!("Magic11111111111111111111111111111111111111");
