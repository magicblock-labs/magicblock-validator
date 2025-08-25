use clap::{Args, ValueEnum};
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};
use strum::Display;

use crate::errors::{ConfigError, ConfigResult};

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
    ///
    #[serde(rename = "resume-strategy")]
    #[serde(default)]
    #[command(flatten)]
    pub resume_strategy_config: LedgerResumeStrategyConfig,
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

impl LedgerConfig {
    pub fn resume_strategy(&self) -> LedgerResumeStrategy {
        match self.resume_strategy_config.kind {
            LedgerResumeStrategyType::Reset => LedgerResumeStrategy::Reset {
                slot: self
                    .resume_strategy_config
                    .reset_slot
                    .unwrap_or_default(),
                keep_accounts: self
                    .resume_strategy_config
                    .keep_accounts
                    .unwrap_or_default(),
            },
            LedgerResumeStrategyType::ResumeOnly => {
                LedgerResumeStrategy::Resume { replay: false }
            }
            LedgerResumeStrategyType::Replay => {
                LedgerResumeStrategy::Resume { replay: true }
            }
        }
    }
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            resume_strategy_config: LedgerResumeStrategyConfig::default(),
            skip_keypair_match_check: false,
            path: Default::default(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
        }
    }
}

impl From<LedgerResumeStrategy> for LedgerResumeStrategyConfig {
    fn from(strategy: LedgerResumeStrategy) -> Self {
        match strategy {
            LedgerResumeStrategy::Reset {
                slot,
                keep_accounts,
            } => LedgerResumeStrategyConfig {
                kind: LedgerResumeStrategyType::Reset,
                reset_slot: Some(slot),
                keep_accounts: Some(keep_accounts),
                account_hydration_concurrency: default_cloning_concurrency(),
            },
            LedgerResumeStrategy::Resume { replay } => {
                LedgerResumeStrategyConfig {
                    kind: if replay {
                        LedgerResumeStrategyType::Replay
                    } else {
                        LedgerResumeStrategyType::ResumeOnly
                    },
                    reset_slot: None,
                    keep_accounts: None,
                    account_hydration_concurrency: default_cloning_concurrency(
                    ),
                }
            }
        }
    }
}

#[clap_prefix("ledger-resume-strategy")]
#[clap_from_serde]
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args, Mergeable,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LedgerResumeStrategyConfig {
    #[derive_env_var]
    #[serde(default)]
    pub kind: LedgerResumeStrategyType,
    #[derive_env_var]
    #[clap_from_serde_skip]
    #[serde(default)]
    pub reset_slot: Option<u64>,
    #[derive_env_var]
    #[clap_from_serde_skip]
    #[serde(default)]
    pub keep_accounts: Option<bool>,
    /// The number of threads to use for cloning accounts during replay hydration.
    #[derive_env_var]
    #[serde(default = "default_cloning_concurrency")]
    pub account_hydration_concurrency: usize,
}

impl LedgerResumeStrategyConfig {
    pub fn validate_resume_strategy(&self) -> ConfigResult<()> {
        use LedgerResumeStrategyType::*;
        match self.kind {
            Replay | ResumeOnly if self.reset_slot.is_some() || self.keep_accounts.is_some() => {
                Err(ConfigError::InvalidResumeStrategy(
                    "reset-slot and keep-accounts are only allowed when resume-strategy is reset"
                        .to_string(),
                ))
            }
            _ => Ok(()),
        }
    }
}

impl Default for LedgerResumeStrategyConfig {
    fn default() -> Self {
        Self {
            kind: LedgerResumeStrategyType::default(),
            reset_slot: None,
            keep_accounts: None,
            account_hydration_concurrency: default_cloning_concurrency(),
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
pub enum LedgerResumeStrategyType {
    Reset,
    ResumeOnly,
    #[default]
    Replay,
}

/// Validated strategies with the relevant arguments
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerResumeStrategy {
    /// Reset the ledger and optionally the accountsdb.
    Reset { slot: u64, keep_accounts: bool },
    /// Resume from the last slot found in the ledger.
    Resume { replay: bool },
}

impl LedgerResumeStrategy {
    pub fn is_resuming(&self) -> bool {
        matches!(self, Self::Resume { .. })
    }

    pub fn is_removing_ledger(&self) -> bool {
        matches!(self, Self::Reset { .. })
    }

    pub fn is_removing_accountsdb(&self) -> bool {
        matches!(
            self,
            Self::Reset {
                keep_accounts: false,
                ..
            }
        )
    }

    pub fn is_replaying(&self) -> bool {
        matches!(self, Self::Resume { replay: true })
    }

    pub fn should_override_bank_slot(&self) -> bool {
        matches!(self, Self::Reset { .. })
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
    fn test_resume_strategy_validate() {
        let test_cases = vec![
            (LedgerResumeStrategyType::Replay, None, None, true),
            (LedgerResumeStrategyType::Replay, Some(1), None, false),
            (LedgerResumeStrategyType::Replay, Some(1), Some(true), false),
            (LedgerResumeStrategyType::Replay, None, Some(false), false),
            (LedgerResumeStrategyType::ResumeOnly, None, None, true),
            (LedgerResumeStrategyType::ResumeOnly, Some(1), None, false),
            (
                LedgerResumeStrategyType::ResumeOnly,
                Some(1),
                Some(true),
                false,
            ),
            (
                LedgerResumeStrategyType::ResumeOnly,
                None,
                Some(false),
                false,
            ),
            (LedgerResumeStrategyType::Reset, None, None, true),
            (LedgerResumeStrategyType::Reset, Some(1), None, true),
            (LedgerResumeStrategyType::Reset, Some(1), Some(true), true),
            (LedgerResumeStrategyType::Reset, None, Some(false), true),
        ];

        for (resume_strategy_type, reset_slot, keep_accounts, is_valid) in
            test_cases
        {
            let config = LedgerResumeStrategyConfig {
                kind: resume_strategy_type,
                reset_slot,
                keep_accounts,
                account_hydration_concurrency: default_cloning_concurrency(),
            };

            assert_eq!(config.validate_resume_strategy().is_ok(), is_valid);
        }
    }

    #[test]
    fn test_merge_with_default() {
        let mut config = LedgerConfig {
            resume_strategy_config: LedgerResumeStrategyConfig {
                kind: LedgerResumeStrategyType::Replay,
                reset_slot: None,
                keep_accounts: None,
                account_hydration_concurrency: 20,
            },
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
    fn test_ledger_merge_default_with_non_default() {
        let mut config = LedgerConfig::default();
        let other = LedgerConfig {
            resume_strategy_config: LedgerResumeStrategyConfig {
                kind: LedgerResumeStrategyType::Reset,
                reset_slot: Some(1),
                keep_accounts: Some(true),
                account_hydration_concurrency: 20,
            },
            skip_keypair_match_check: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_ledger_merge_non_default() {
        let mut config = LedgerConfig {
            resume_strategy_config: LedgerResumeStrategyConfig {
                kind: LedgerResumeStrategyType::Reset,
                reset_slot: Some(1),
                keep_accounts: Some(true),
                account_hydration_concurrency: 20,
            },
            skip_keypair_match_check: true,
            path: Some("ledger.example.com".to_string()),
            size: 1000000000,
        };
        let original_config = config.clone();
        let other = LedgerConfig {
            resume_strategy_config: LedgerResumeStrategyConfig {
                kind: LedgerResumeStrategyType::ResumeOnly,
                reset_slot: None,
                keep_accounts: None,
                account_hydration_concurrency: 150,
            },
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
resume-strategy = { kind = "replay", reset-slot = 0, keep-accounts = true }
skip-keypair-match-check = true
path = "ledger.example.com"
size = 1000000000
"#;

        let config: EphemeralConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ledger,
            LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::Replay,
                    reset_slot: Some(0),
                    keep_accounts: Some(true),
                    account_hydration_concurrency: default_cloning_concurrency(
                    ),
                },
                skip_keypair_match_check: true,
                path: Some("ledger.example.com".to_string()),
                size: 1000000000,
            }
        );

        let toml_str = r#"
[ledger]
resume-strategy = { kind = "resume-only" }
size = 1000000000
"#;

        let config: EphemeralConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ledger,
            LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::ResumeOnly,
                    reset_slot: None,
                    keep_accounts: None,
                    account_hydration_concurrency: default_cloning_concurrency(
                    ),
                },
                skip_keypair_match_check: false,
                path: None,
                size: 1000000000,
            }
        );

        let toml_str = r#"
[ledger]
resume-strategy = { kind = "reset" }
size = 1000000000
"#;

        let config: EphemeralConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.ledger,
            LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::Reset,
                    reset_slot: None,
                    keep_accounts: None,
                    account_hydration_concurrency: default_cloning_concurrency(
                    ),
                },
                skip_keypair_match_check: false,
                path: None,
                size: 1000000000,
            }
        );
    }
}
