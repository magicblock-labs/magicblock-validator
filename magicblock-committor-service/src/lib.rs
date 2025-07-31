mod compute_budget;
pub mod config;
mod consts;
pub mod error;
pub mod persist;
mod pubkeys_provider;
mod service;
pub mod service_ext;
pub mod transactions;
pub mod types;

pub mod intent_execution_manager;
// TODO(edwin): define visibility
mod committor_processor;
pub mod intent_executor;
#[cfg(feature = "dev-context-only-utils")]
pub mod stubs;
pub mod tasks;
pub mod transaction_preperator;
pub mod utils;
// TODO(edwin) pub(crate)

pub use compute_budget::ComputeBudgetConfig;
pub use magicblock_committor_program::{
    ChangedAccount, Changeset, ChangesetMeta,
};
pub use service::{BaseIntentCommittor, CommittorService};
pub fn changeset_for_slot(slot: u64) -> Changeset {
    Changeset {
        slot,
        ..Changeset::default()
    }
}
