use clap::{Args, ValueEnum};
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};
use strum::Display;

// Default desired ledger size 100 GiB
pub const DEFAULT_LEDGER_SIZE_BYTES: u64 = 100 * 1024 * 1024 * 1024;

#[clap_prefix("ledger")]
#[clap_from_serde]
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args, Mergeable,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LedgerConfig {
    /// The strategy to use for resuming the ledger.
    /// Reset will remove the existing ledger.
    /// Resume only will remove the ledger and resume from the last slot.
    /// Replay and resume will preserve the existing ledger and replay it and then resume.
    #[derive_env_var]
    #[serde(default)]
    pub resume_strategy: LedgerResumeStrategy,
    /// Checks that the validator keypair matches the one in the ledger.
    #[derive_env_var]
    #[arg(
        help = "Whether to check that the validator keypair matches the one in the ledger."
    )]
    #[serde(default)]
    pub skip_keypair_match_check: bool,
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
            resume_strategy: LedgerResumeStrategy::default(),
            skip_keypair_match_check: false,
            path: Default::default(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
            replay: ReplayConfig::default(),
        }
    }
}

#[clap_prefix("replay")]
#[clap_from_serde]
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args, Mergeable,
)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    /// The number of threads to use for cloning accounts.
    #[derive_env_var]
    #[serde(default = "default_cloning_concurrency")]
    pub hydration_concurrency: usize,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            hydration_concurrency: default_cloning_concurrency(),
        }
    }
}

#[derive(
    Debug,
    Display,
    Clone,
    Default,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum LedgerResumeStrategy {
    #[default]
    Reset,
    ResumeOnly,
    Replay,
}

impl LedgerResumeStrategy {
    pub fn is_resuming(&self) -> bool {
        self != &Self::Reset
    }

    pub fn is_removing_ledger(&self) -> bool {
        self != &Self::Replay
    }

    pub fn is_replaying(&self) -> bool {
        self == &Self::Replay
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
    use crate::EphemeralConfig;

    #[test]
    fn test_ledger_merge_with_default() {
        let mut config = LedgerConfig {
            resume_strategy: LedgerResumeStrategy::Replay,
            skip_keypair_match_check: true,
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
            resume_strategy: LedgerResumeStrategy::Replay,
            skip_keypair_match_check: true,
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
            resume_strategy: LedgerResumeStrategy::Replay,
            skip_keypair_match_check: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
            replay: ReplayConfig {
                hydration_concurrency: 20,
            },
        };
        let original_config = config.clone();
        let other = LedgerConfig {
            resume_strategy: LedgerResumeStrategy::ResumeOnly,
            skip_keypair_match_check: true,
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

    #[test]
    fn test_serde() {
        let toml_str = r#"
[ledger]
resume-strategy = "replay"
skip-keypair-match-check = true
path = "ledger.example.com"
size = 1000000000
"#;

        let config: EphemeralConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ledger,
            LedgerConfig {
                resume_strategy: LedgerResumeStrategy::Replay,
                skip_keypair_match_check: true,
                path: Some("ledger.example.com".to_string()),
                size: 1000000000,
                replay: ReplayConfig::default(),
            }
        );

        let toml_str = r#"
[ledger]
resume-strategy = "resume-only"
size = 1000000000
"#;

        let config: EphemeralConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ledger,
            LedgerConfig {
                resume_strategy: LedgerResumeStrategy::ResumeOnly,
                skip_keypair_match_check: false,
                path: None,
                size: 1000000000,
                replay: ReplayConfig::default(),
            }
        );

        let toml_str = r#"
[ledger]
resume-strategy = "reset"
size = 1000000000
"#;

        let config: EphemeralConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ledger,
            LedgerConfig {
                resume_strategy: LedgerResumeStrategy::Reset,
                skip_keypair_match_check: false,
                path: None,
                size: 1000000000,
                replay: ReplayConfig::default(),
            }
        );
    }
}
