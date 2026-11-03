#![doc = include_str!("../README.md")]
#![allow(clippy::manual_is_multiple_of)]

use solana_pubkey::declare_id;

pub mod consts;
pub mod error;
pub mod instruction;
pub mod instruction_builder;
pub mod instruction_chunks;
pub mod pdas;
mod state;

pub use state::{
    changeset::{
        ChangedAccount, ChangedAccountMeta, ChangedBundle, Changeset, ChangesetBundles,
        ChangesetMeta, CommitableAccount,
    },
    changeset_chunks::{ChangesetChunk, ChangesetChunks},
    chunks::Chunks,
};

declare_id!("ComtrB2KEaWgXsW1dhr1xYL4Ht4Bjj3gXnnL6KMdABq");

#[macro_export]
macro_rules! compute {
    ($msg:expr=> $($tt:tt)*) => {
        ::solana_program::msg!(concat!($msg, " {"));
        ::solana_program::log::sol_log_compute_units();
        $($tt)*
        ::solana_program::log::sol_log_compute_units();
        ::solana_program::msg!(concat!(" } // ", $msg));
    };
}
