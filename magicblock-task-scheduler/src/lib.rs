pub mod db;
pub mod errors;
mod hydra;
pub mod service;

pub use db::SchedulerDatabase;
pub use errors::TaskSchedulerError;
/// Flat per-execution reward (lamports) that hydra pays the external cranker.
pub use hydra::CRANKER_REWARD;
/// Program id of the ephemeral hydra crank program that the scheduler targets.
pub use hydra::EPHEMERAL_PROGRAM_ID as HYDRA_EPHEMERAL_PROGRAM_ID;
/// Derives the on-chain hydra crank account address for a task from its
/// `(authority, task_id)` — the deterministic, per-authority namespace the
/// scheduler uses. Tests and tooling can use this to locate the crank account a
/// schedule request created.
pub use service::crank_pubkey;
pub use service::TaskSchedulerService;
