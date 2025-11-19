#![allow(deprecated)]
#![allow(unexpected_cfgs)]
use solana_program::declare_id;

pub mod instruction;
mod processor;
pub mod state;
mod utils;

pub use processor::process;

declare_id!("f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process);

pub use ephemeral_rollups_sdk::id as delegation_program_id;
