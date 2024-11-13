use solana_program::{declare_id, entrypoint};

pub mod instruction;
mod processor;
pub mod state;
mod utils;

pub use processor::process;

declare_id!("f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4");

entrypoint!(process);
