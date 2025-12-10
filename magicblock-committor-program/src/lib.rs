#![allow(clippy::manual_is_multiple_of)]

use solana_pubkey::declare_id;
pub mod consts;
pub mod error;
pub mod instruction;
pub mod instruction_chunks;
pub mod pdas;
mod state;

pub mod instruction_builder;
mod processor;
mod utils;

// #[cfg(not(feature = "no-entrypoint"))]
pub use processor::process;
pub use state::{
    changeset::{
        ChangedAccount, ChangedAccountMeta, ChangedBundle, Changeset,
        ChangesetBundles, ChangesetMeta, CommitableAccount,
    },
    changeset_chunks::{ChangesetChunk, ChangesetChunks},
    chunks::Chunks,
};

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process);

declare_id!("ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq");
