#![doc = include_str!("../README.md")]
mod processor;
mod utils;

pub use processor::process;

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process);
