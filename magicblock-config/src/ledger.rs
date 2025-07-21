use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};

use crate::helpers::serde_defaults::bool_true;

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
    #[serde(default)]
    #[command(flatten)]
    pub replay: ReplayConfig,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            reset: bool_true(),
            path: Default::default(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
            replay: ReplayConfig::default(),
        }
    }
}

#[clap_prefix("replay")]
#[clap_from_serde]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    /// The number of threads to use for cloning accounts.
    #[derive_env_var]
    #[serde(default = "default_cloning_concurrency")]
    pub hydration_concurrency: usize,
}

impl ReplayConfig {
    pub fn merge(&mut self, other: Self) {
        let default = Self::default();

        if self.hydration_concurrency == default.hydration_concurrency
            && other.hydration_concurrency != default.hydration_concurrency
        {
            self.hydration_concurrency = other.hydration_concurrency;
        }
    }
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            hydration_concurrency: default_cloning_concurrency(),
        }
    }
}

const fn default_ledger_size() -> u64 {
    DEFAULT_LEDGER_SIZE_BYTES
}

const fn default_cloning_concurrency() -> usize {
    10
}

#[cfg(test)]
mod tests {
    use magicblock_config_helpers::Merge;

    use super::*;

    #[test]
    fn test_ledger_merge_with_default() {
        let mut config = LedgerConfig {
            reset: false,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
            replay: ReplayConfig {
                hydration_concurrency: 20,
            },
        };
        let original_config = config.clone();
        let other = LedgerConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_ledger_merge_default_with_non_default() {
        let mut config = LedgerConfig::default();
        let other = LedgerConfig {
            reset: false,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
            replay: ReplayConfig {
                hydration_concurrency: 20,
            },
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_ledger_merge_non_default() {
        let mut config = LedgerConfig {
            reset: false,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
            replay: ReplayConfig {
                hydration_concurrency: 20,
            },
        };
        let original_config = config.clone();
        let other = LedgerConfig {
            reset: true,
            path: Some("ledger2.example.com".to_string()),
            size: 10000,
            replay: ReplayConfig {
                hydration_concurrency: 150,
            },
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_replay_merge_with_default() {
        let mut config = ReplayConfig {
            hydration_concurrency: 20,
        };
        let original_config = config.clone();
        let other = ReplayConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_replay_merge_default_with_non_default() {
        let mut config = ReplayConfig::default();
        let other = ReplayConfig {
            hydration_concurrency: 20,
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_replay_merge_non_default() {
        let mut config = ReplayConfig {
            hydration_concurrency: 20,
        };
        let original_config = config.clone();
        let other = ReplayConfig {
            hydration_concurrency: 150,
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }
}
