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
    /// The slot to start from.
    /// If left empty it will start from the last slot found in the ledger and default to 0.
    #[derive_env_var]
    #[clap_from_serde_skip]
    #[serde(default)]
    pub starting_slot: Option<u64>,
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
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            resume_strategy: LedgerResumeStrategy::default(),
            starting_slot: None,
            skip_keypair_match_check: false,
            path: Default::default(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
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
    Reset,
    DiscardResume,
    AccountsOnly,
    ResumeOnly,
    #[default]
    Replay,
}

impl LedgerResumeStrategy {
    /// Whether the ledger should be resumed.
    /// This assumes that a ledger exists.
    pub fn is_resuming(&self) -> bool {
        matches!(self, Self::DiscardResume | Self::ResumeOnly | Self::Replay)
    }

    pub fn is_removing_ledger(&self) -> bool {
        matches!(self, Self::Reset | Self::DiscardResume | Self::AccountsOnly)
    }

    pub fn is_removing_accountsdb(&self) -> bool {
        matches!(self, Self::Reset | Self::DiscardResume)
    }

    pub fn is_removing_validator_keypair(&self) -> bool {
        matches!(self, Self::Reset | Self::DiscardResume | Self::AccountsOnly)
    }

    pub fn is_replaying(&self) -> bool {
        matches!(self, Self::Replay)
    }

    pub fn should_override_bank_slot(&self) -> bool {
        // Strategies that resume but remove the ledger must override, or the start slot will be 0
        matches!(self, Self::DiscardResume)
    }
}

const fn default_ledger_size() -> u64 {
    DEFAULT_LEDGER_SIZE_BYTES
}

#[cfg(test)]
mod tests {
    use magicblock_config_helpers::Merge;

    use super::*;
    use crate::EphemeralConfig;

    #[test]
    fn test_merge_with_default() {
        let mut config = LedgerConfig {
            resume_strategy: LedgerResumeStrategy::Replay,
            starting_slot: None,
            skip_keypair_match_check: true,
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
            resume_strategy: LedgerResumeStrategy::Replay,
            starting_slot: None,
            skip_keypair_match_check: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = LedgerConfig {
            resume_strategy: LedgerResumeStrategy::AccountsOnly,
            starting_slot: None,
            skip_keypair_match_check: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
        };
        let original_config = config.clone();
        let other = LedgerConfig {
            resume_strategy: LedgerResumeStrategy::ResumeOnly,
            starting_slot: None,
            skip_keypair_match_check: true,
            path: Some("ledger2.example.com".to_string()),
            size: 10000,
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_serde() {
        let toml_str = r#"
[ledger]
resume-strategy = "replay"
starting-slot = 0
skip-keypair-match-check = true
path = "ledger.example.com"
size = 1000000000
"#;

        let config: EphemeralConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ledger,
            LedgerConfig {
                resume_strategy: LedgerResumeStrategy::Replay,
                starting_slot: Some(0),
                skip_keypair_match_check: true,
                path: Some("ledger.example.com".to_string()),
                size: 1000000000,
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
                starting_slot: None,
                skip_keypair_match_check: false,
                path: None,
                size: 1000000000,
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
                starting_slot: None,
                skip_keypair_match_check: false,
                path: None,
                size: 1000000000,
            }
        );
    }
}
