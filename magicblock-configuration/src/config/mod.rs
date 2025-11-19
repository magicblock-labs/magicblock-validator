pub mod accounts;
pub mod chain;
pub mod cli;
pub mod ledger;
pub mod program;
pub mod scheduler;
pub mod validator;

// Re-export types for backward compatibility and easier access
pub use accounts::{AccountsDbConfig, BlockSize};
pub use chain::{ChainLinkConfig, ChainOperationConfig, CommitStrategy};
pub use ledger::LedgerConfig;
pub use program::LoadableProgram;
pub use scheduler::TaskSchedulerConfig;
use serde::{Deserialize, Serialize};
pub use validator::ValidatorConfig;

#[derive(
    clap::ValueEnum, Debug, Clone, Default, PartialEq, Deserialize, Serialize,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum LifecycleMode {
    #[default]
    Ephemeral,
    Replica,
    Offline,
    ProgramsReplica,
}
