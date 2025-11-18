use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(
    ValueEnum, Debug, Clone, Default, PartialEq, Deserialize, Serialize,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum LifecycleMode {
    /// - clone all accounts
    /// - write to all accounts
    Replica,
    /// - clone program accounts
    /// - write to all accounts
    ProgramsReplica,
    /// - clone all accounts
    /// - write to delegated accounts
    #[default]
    Ephemeral,
    /// - clone no accounts
    /// - write to all accounts
    Offline,
}

impl LifecycleMode {
    /// Check whether the validator lifecycle implies
    /// sync with remote source of blockchain state
    pub fn needs_remote_account_provider(&self) -> bool {
        !matches!(self, LifecycleMode::Offline)
    }
}
