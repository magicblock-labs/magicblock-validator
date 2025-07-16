use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};

use crate::helpers::serde_defaults::{bool_false, bool_true};

// Default desired ledger size 100 GiB
pub const DEFAULT_LEDGER_SIZE_BYTES: u64 = 100 * 1024 * 1024 * 1024;

#[clap_prefix("ledger")]
#[clap_from_serde]
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args, Mergeable,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LedgerConfig {
    /// If a previous ledger is found it is removed before starting the validator
    /// This can be disabled by setting [Self::reset] to `false`.
    #[derive_env_var]
    #[arg(help = "Whether to reset the ledger before starting the validator.")]
    #[serde(default = "bool_true")]
    pub reset: bool,
    /// If a previous ledger is found it is removed before starting the validator
    /// This can be disabled by setting [Self::reset] to `false`.
    #[derive_env_var]
    #[arg(help = "Whether to skip replay of the ledger.")]
    #[serde(default = "bool_false")]
    pub skip_replay: bool,
    /// The file system path onto which the ledger should be written at
    /// If left empty it will be auto-generated to a temporary folder
    #[derive_env_var]
    #[clap_from_serde_skip]
    #[arg(
        help = "The file system path onto which the ledger should be written at."
    )]
    #[serde(default)]
    pub path: Option<String>,
    /// The size under which it's desired to keep ledger in bytes.
    #[derive_env_var]
    #[arg(help = "The size under which it's desired to keep ledger in bytes.")]
    #[serde(default = "default_ledger_size")]
    pub size: u64,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            reset: bool_true(),
            skip_replay: bool_false(),
            path: Default::default(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
        }
    }
}

const fn default_ledger_size() -> u64 {
    DEFAULT_LEDGER_SIZE_BYTES
}

#[cfg(test)]
mod tests {
    use magicblock_config_helpers::Merge;

    use super::*;

    #[test]
    fn test_merge_with_default() {
        let mut config = LedgerConfig {
            reset: false,
            skip_replay: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
        };
        let original_config = config.clone();
        let other = LedgerConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_merge_default_with_non_default() {
        let mut config = LedgerConfig::default();
        let other = LedgerConfig {
            reset: false,
            skip_replay: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = LedgerConfig {
            reset: false,
            skip_replay: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
        };
        let original_config = config.clone();
        let other = LedgerConfig {
            reset: true,
            skip_replay: false,
            path: Some("ledger2.example.com".to_string()),
            size: 10000,
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }
}
