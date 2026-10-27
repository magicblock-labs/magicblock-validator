#![doc = include_str!("../README.md")]

mod config;
mod execution;
mod intake;
mod service;
mod shared;
#[cfg(test)]
mod test_utils;

pub use config::{CommittorConfig, DEFAULT_ACTIONS_TIMEOUT};
pub use intake::{CommitIdProvider, is_intent_within_limits};
pub use service::CommittorService;
pub use shared::{
    account_state::{AccountSnapshot, TaskInfoProviderError, TaskInfoProviderResult},
    compute_budget::ComputeBudgetConfig,
};
