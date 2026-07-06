//! Read-only access to the legacy ledger.
//!
//! The engine owns block production and its own ledger; this crate is retained
//! only to read data written by earlier validator versions, and is consumed by
//! `magicblock-aperture` as a fallback for RPC reads the engine's ledger cannot
//! serve. It is expected to be removed once the transition completes.

pub use database::{
    meta::PerfSample,
    options::{BLOCKSTORE_DIRECTORY_ROCKS_LEVEL, LedgerOptions},
};
pub use store::api::{Ledger, SignatureInfosForAddress};

mod database;
pub mod errors;
mod metrics;
mod store;
