use std::str::FromStr;

use clap::{Args, ValueEnum};
use magicblock_config_macro::{clap_from_serde, clap_prefix};
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use strum::Display;
use strum_macros::EnumString;
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
    #[arg(help = "The max number of accounts to monitor.")]
    #[serde(default = "default_max_monitored_accounts")]
    pub max_monitored_accounts: usize,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            remote: Default::default(),
            lifecycle: Default::default(),
            commit: Default::default(),
            allowed_programs: Default::default(),
            db: Default::default(),
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
    #[serde(untagged)]
    Custom,
    #[serde(untagged)]
    CustomWithWs,
    #[serde(untagged)]
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

// fn allowed_program_parser(s: &str) -> Result<Vec<AllowedProgram>, String> {
//     let parts: Vec<String> =
//         s.split(':').map(|part| part.to_string()).collect();
//     let [id, path] = parts.as_slice() else {
//         return Err(format!("Invalid program config: {}", s));
//     };
//     let id = Pubkey::from_str(id)
//         .map_err(|e| format!("Invalid program id {}: {}", id, e))?;
// }
