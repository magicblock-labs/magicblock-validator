mod bundle_strategy;
mod bundles;
mod commit_info;
mod commit_strategist;
mod compute_budget;
pub mod config;
mod consts;
pub mod error;
mod finalize;
pub mod persist;
mod pubkeys_provider;
mod service;
mod transactions;
mod types;
mod undelegate;

pub mod commit_scheduler;
// TODO(edwin): define visibility
mod committor_processor;
pub mod l1_message_executor;
#[cfg(feature = "dev-context-only-utils")]
pub mod stubs;
pub mod tasks;
pub mod transaction_preperator;
pub(crate) mod utils;

pub use commit_info::CommitInfo;
pub use compute_budget::ComputeBudgetConfig;
pub use magicblock_committor_program::{
    ChangedAccount, Changeset, ChangesetMeta,
};
pub use service::{CommittorService, L1MessageCommittor};
pub fn changeset_for_slot(slot: u64) -> Changeset {
    Changeset {
        slot,
        ..Changeset::default()
    }
}
