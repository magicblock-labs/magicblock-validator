pub mod crank;
pub mod errors;
pub mod service;

/// Derives the on-chain hydra crank account address for a task from its
/// `(authority, task_id)` — the deterministic, per-authority namespace the
/// scheduler uses. Tests and tooling can use this to locate the crank account a
/// schedule request created.
pub use crank::crank_pubkey;
pub use errors::TaskSchedulerError;
pub use service::TaskSchedulerService;
