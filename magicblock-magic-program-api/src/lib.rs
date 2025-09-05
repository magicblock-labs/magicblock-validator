pub mod args;
pub mod instruction;

pub use solana_program::{declare_id, pubkey, pubkey::Pubkey};

declare_id!("Magic11111111111111111111111111111111111111");

pub const MAGIC_CONTEXT_PUBKEY: Pubkey =
    pubkey!("MagicContext1111111111111111111111111111111");

/// We believe 5MB should be enough to store all scheduled commits within a
/// slot. Once we store more data in the magic context we need to reconsicer
/// this size.
/// NOTE: the default max accumulated account size per transaction is 64MB.
/// See: MAX_LOADED_ACCOUNTS_DATA_SIZE_BYTES inside program-runtime/src/compute_budget_processor.rs
pub const MAGIC_CONTEXT_SIZE: usize = 1024 * 1024 * 5; // 5 MB

pub const TASK_CONTEXT_PUBKEY: Pubkey =
    pubkey!("TaskContext11111111111111111111111111111111");
