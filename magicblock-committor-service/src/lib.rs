mod bundle_strategy;
mod bundles;
mod commit;
mod commit_info;
mod commit_stage;
mod commit_strategy;
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

#[cfg(feature = "dev-context-only-utils")]
pub mod stubs;

pub use commit_info::CommitInfo;
pub use commit_stage::CommitStage;
pub use compute_budget::ComputeBudgetConfig;
pub use magicblock_committor_program::{
    ChangedAccount, Changeset, ChangesetMeta,
};
pub use service::{ChangesetCommittor, CommittorService};
pub fn changeset_for_slot(slot: u64) -> Changeset {
    Changeset {
        slot,
        ..Changeset::default()
    }
}
