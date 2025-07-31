use solana_program::declare_id;

pub mod instruction;
mod magic_program;
mod processor;
pub mod state;
mod utils;

pub use processor::process;

declare_id!("6CtBV7Mrejs2CpFUW3V1Hkpzck4LoCxPTunbxQY6QsxC");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process);

pub use ephemeral_rollups_sdk::id as delegation_program_id;
