use std::str::FromStr;

use clap::{Args, ValueEnum};
use magicblock_config_macro::{clap_from_serde, clap_prefix};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use strum::{Display, EnumString};
use url::Url;

use crate::accounts_db::AccountsDbConfig;

// -----------------
// AccountsConfig
// -----------------
#[clap_prefix("accounts")]
#[clap_from_serde]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AccountsConfig {
    #[serde(default)]
    #[command(flatten)]
    pub remote: RemoteConfig,
    #[derive_env_var]
    #[arg(help = "The lifecycle mode to use.")]
    #[serde(default)]
    pub lifecycle: LifecycleMode,
    #[serde(default)]
    #[command(flatten)]
    pub commit: CommitStrategy,
    #[clap_from_serde_skip]
    #[arg(help = "The list of allowed programs to load.")]
    #[serde(default)]
    pub allowed_programs: Vec<AllowedProgram>,
    #[serde(default)]
    #[command(flatten)]
    pub db: AccountsDbConfig,
    #[serde(default)]
    #[command(flatten)]
    pub clone: AccountsCloneConfig,
    #[arg(help = "The max number of accounts to monitor.")]
    #[serde(default = "default_max_monitored_accounts")]
    pub max_monitored_accounts: usize,
}

impl AccountsConfig {
    pub fn merge(&mut self, other: AccountsConfig) {
        let default = Self::default();

        if self.remote == default.remote && other.remote != default.remote {
            self.remote = other.remote;
        }
        if self.lifecycle == default.lifecycle
            && other.lifecycle != default.lifecycle
        {
            self.lifecycle = other.lifecycle;
        }
        if self.commit == default.commit && other.commit != default.commit {
            self.commit = other.commit;
        }
        if self.allowed_programs == default.allowed_programs
            && other.allowed_programs != default.allowed_programs
        {
            self.allowed_programs = other.allowed_programs;
        }
        if self.db == default.db && other.db != default.db {
            self.db = other.db;
        }
        if self.clone == default.clone && other.clone != default.clone {
            self.clone = other.clone;
        }
        if self.max_monitored_accounts == default.max_monitored_accounts
            && other.max_monitored_accounts != default.max_monitored_accounts
        {
            self.max_monitored_accounts = other.max_monitored_accounts;
        }
    }
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            remote: Default::default(),
            lifecycle: Default::default(),
            commit: Default::default(),
            allowed_programs: Default::default(),
            db: Default::default(),
            clone: Default::default(),
            max_monitored_accounts: default_max_monitored_accounts(),
        }
    }
}
// -----------------
// RemoteConfig
// -----------------
#[clap_prefix("remote")]
#[clap_from_serde]
#[derive(
    Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize, Args,
)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    #[arg(help = "The predefined cluster to use.")]
    #[serde(default)]
    pub cluster: RemoteCluster,
    #[derive_env_var]
    #[arg(help = "The URL to use for the custom cluster.")]
    #[serde(default)]
    #[clap_from_serde_skip]
    pub url: Option<Url>,
    #[derive_env_var]
    #[clap_from_serde_skip]
    #[arg(help = "The WebSocket URLs to use for the custom cluster.")]
    #[serde(default)]
    pub ws_url: Option<Vec<Url>>,
}

// -----------------
// RemoteConfigType
// -----------------
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
#[clap(rename_all = "kebab-case")]
pub enum RemoteCluster {
    #[default]
    Devnet,
    #[serde(alias = "mainnet-beta")]
    Mainnet,
    Testnet,
    #[serde(alias = "local")]
    #[serde(alias = "localhost")]
    Development,
    Custom,
    CustomWithWs,
    CustomWithMultipleWs,
}

// -----------------
// LifecycleMode
// -----------------
#[derive(
    Debug,
    Clone,
    Display,
    Default,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    EnumString,
    ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum LifecycleMode {
    Replica,
    #[default]
    ProgramsReplica,
    Ephemeral,
    Offline,
}

// -----------------
// CommitStrategy
// -----------------
#[clap_prefix("commit")]
#[clap_from_serde]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args)]
#[serde(deny_unknown_fields)]
pub struct CommitStrategy {
    #[derive_env_var]
    #[serde(default = "default_frequency_millis")]
    pub frequency_millis: u64,
    /// The compute unit price offered when we send the commit account transaction
    /// This is in micro lamports and defaults to `1_000_000` (1 Lamport)
    #[derive_env_var]
    #[serde(default = "default_compute_unit_price")]
    pub compute_unit_price: u64,
}

fn default_frequency_millis() -> u64 {
    500
}

fn default_max_monitored_accounts() -> usize {
    2048
}

fn default_compute_unit_price() -> u64 {
    // This is the lowest we found to pass the transactions through mainnet fairly
    // consistently
    1_000_000 // 1_000_000 micro-lamports == 1 Lamport
}

impl Default for CommitStrategy {
    fn default() -> Self {
        Self {
            frequency_millis: default_frequency_millis(),
            compute_unit_price: default_compute_unit_price(),
        }
    }
}

// -----------------
// PrepareLookupTables
// -----------------
#[derive(
    Debug,
    Clone,
    Display,
    Default,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    EnumString,
    ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum PrepareLookupTables {
    Always,
    #[default]
    Never,
}

// -----------------
// AccountsCloneConfig
// -----------------
#[clap_prefix("clone")]
#[clap_from_serde]
#[derive(
    Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize, Args,
)]
#[serde(deny_unknown_fields)]
pub struct AccountsCloneConfig {
    #[serde(default)]
    pub prepare_lookup_tables: PrepareLookupTables,
}

#[derive(
    Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, Args,
)]
#[serde(deny_unknown_fields)]
pub struct AllowedProgram {
    #[serde(
        deserialize_with = "pubkey_deserialize",
        serialize_with = "pubkey_serialize"
    )]
    pub id: Pubkey,
}

impl FromStr for AllowedProgram {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id = Pubkey::from_str(s)
            .map_err(|e| format!("Invalid program id {s}: {e}"))?;
        Ok(AllowedProgram { id })
    }
}

fn pubkey_deserialize<'de, D>(deserializer: D) -> Result<Pubkey, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Pubkey::from_str(&s).map_err(serde::de::Error::custom)
}

fn pubkey_serialize<S>(key: &Pubkey, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    key.to_string().serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockSize;

    #[test]
    fn test_merge_with_default() {
        let mut config = AccountsConfig {
            remote: RemoteConfig {
                cluster: RemoteCluster::Custom,
                url: Some(Url::parse("http://0.0.0.0:7799").unwrap()),
                ws_url: None,
            },
            lifecycle: LifecycleMode::Ephemeral,
            commit: CommitStrategy {
                frequency_millis: 123,
                compute_unit_price: 123,
            },
            allowed_programs: vec![AllowedProgram {
                id: Pubkey::from_str(
                    "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4",
                )
                .unwrap(),
            }],
            db: AccountsDbConfig::default(),
            clone: AccountsCloneConfig::default(),
            max_monitored_accounts: 123,
        };
        let original_config = config.clone();
        let other = AccountsConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_merge_default_with_non_default() {
        let mut config = AccountsConfig::default();
        let other = AccountsConfig {
            remote: RemoteConfig {
                cluster: RemoteCluster::Custom,
                url: Some(Url::parse("http://0.0.0.0:7799").unwrap()),
                ws_url: None,
            },
            lifecycle: LifecycleMode::Ephemeral,
            commit: CommitStrategy {
                frequency_millis: 123,
                compute_unit_price: 123,
            },
            allowed_programs: vec![AllowedProgram {
                id: Pubkey::from_str(
                    "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4",
                )
                .unwrap(),
            }],
            db: AccountsDbConfig::default(),
            clone: AccountsCloneConfig::default(),
            max_monitored_accounts: 123,
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = AccountsConfig {
            remote: RemoteConfig {
                cluster: RemoteCluster::Custom,
                url: Some(Url::parse("http://0.0.0.0:7999").unwrap()),
                ws_url: Some(vec![Url::parse("wss://0.0.0.0:7999").unwrap()]),
            },
            lifecycle: LifecycleMode::Offline,
            commit: CommitStrategy {
                frequency_millis: 1234,
                compute_unit_price: 1234,
            },
            allowed_programs: vec![AllowedProgram {
                id: Pubkey::from_str(
                    "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4",
                )
                .unwrap(),
            }],
            db: AccountsDbConfig {
                db_size: 1233,
                block_size: BlockSize::Block512,
                index_map_size: 1233,
                max_snapshots: 1233,
                snapshot_frequency: 1233,
            },
            clone: AccountsCloneConfig::default(),
            max_monitored_accounts: 1233,
        };
        let original_config = config.clone();
        let other = AccountsConfig {
            remote: RemoteConfig {
                cluster: RemoteCluster::Custom,
                url: Some(Url::parse("http://0.0.0.0:7799").unwrap()),
                ws_url: None,
            },
            lifecycle: LifecycleMode::Ephemeral,
            commit: CommitStrategy {
                frequency_millis: 123,
                compute_unit_price: 123,
            },
            allowed_programs: vec![AllowedProgram {
                id: Pubkey::from_str(
                    "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4",
                )
                .unwrap(),
            }],
            db: AccountsDbConfig::default(),
            clone: AccountsCloneConfig::default(),
            max_monitored_accounts: 123,
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_clone_config_default() {
        let config = AccountsCloneConfig::default();
        assert_eq!(config.prepare_lookup_tables, PrepareLookupTables::Never);
    }

    #[test]
    fn test_clone_config_merge() {
        let mut config = AccountsConfig::default();
        let other = AccountsConfig {
            clone: AccountsCloneConfig {
                prepare_lookup_tables: PrepareLookupTables::Always,
            },
            ..Default::default()
        };

        config.merge(other.clone());
        assert_eq!(
            config.clone.prepare_lookup_tables,
            PrepareLookupTables::Always
        );
    }

    #[test]
    fn test_clone_config_serde() {
        let toml_str = r#"
[clone]
prepare_lookup_tables = "always"
"#;

        let config: AccountsConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.clone.prepare_lookup_tables,
            PrepareLookupTables::Always
        );
    }
}
