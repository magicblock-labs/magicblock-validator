pub mod commit_sender;
pub mod errors;
pub mod sleipnir_instruction;
pub mod sleipnir_processor;
mod validator;

pub use sleipnir_core::magic_program::*;
pub use validator::*;
