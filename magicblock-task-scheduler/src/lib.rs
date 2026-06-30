pub mod db;
pub mod errors;
pub mod service;

pub use db::SchedulerDatabase;
pub use errors::TaskSchedulerError;
/// Derives the on-chain hydra crank account address for a task from its
/// `(authority, task_id)` — the deterministic, per-authority namespace the
/// scheduler uses. Tests and tooling can use this to locate the crank account a
/// schedule request created.
pub use service::crank_pubkey;
pub use service::TaskSchedulerService;
