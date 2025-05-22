use std::str::FromStr;

use magicblock_accounts_db::config::AccountsDbConfig;
use serde::{Deserialize, Serialize};
use solana_sdk::{native_token::LAMPORTS_PER_SOL, pubkey::Pubkey};
use strum_macros::EnumString;
use url::Url;

use crate::errors::{ConfigError, ConfigResult};

// -----------------
// AccountsConfig
// -----------------
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AccountsConfig {
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub lifecycle: LifecycleMode,
    #[serde(default)]
    pub commit: CommitStrategy,
    #[serde(default)]
    pub payer: Payer,
    #[serde(default)]
    pub allowed_programs: Vec<AllowedProgram>,

    #[serde(default)]
    pub db: AccountsDbConfig,

    #[serde(default = "default_max_monitored_accounts")]
    pub max_monitored_accounts: usize,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            remote: Default::default(),
            lifecycle: Default::default(),
            commit: Default::default(),
            payer: Default::default(),
            allowed_programs: Default::default(),
            db: Default::default(),
            max_monitored_accounts: default_max_monitored_accounts(),
        }
    }
}
// -----------------
// RemoteConfig
// -----------------
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteConfig {
    #[default]
    Devnet,
    #[serde(alias = "mainnet-beta")]
    Mainnet,
    Testnet,
    #[serde(alias = "local")]
    #[serde(alias = "localhost")]
    Development,
    #[serde(untagged)]
    Custom(Url),
    #[serde(untagged)]
    CustomWithWs(Url, Url),
    #[serde(untagged)]
    CustomWithMutlipleWs {
        http: Url,
        ws: Vec<Url>,
    },
}

// -----------------
// LifecycleMode
// -----------------
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitStrategy {
    #[serde(default = "default_frequency_millis")]
    pub frequency_millis: u64,
    /// The compute unit price offered when we send the commit account transaction
    /// This is in micro lamports and defaults to `1_000_000` (1 Lamport)
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
// Payer
// -----------------
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Payer {
    /// The payer init balance in lamports.
    /// Read it via [Self::try_init_lamports].
    pub init_lamports: Option<u64>,
    /// The payer init balance in SOL.
    /// Read it via [Self::try_init_lamports].
    init_sol: Option<u64>,
}

pub struct PayerParams {
    pub init_lamports: Option<u64>,
    pub init_sol: Option<u64>,
}

impl Payer {
    pub fn new(params: PayerParams) -> Self {
        Self {
            init_lamports: params.init_lamports,
            init_sol: params.init_sol,
        }
    }
    pub fn try_init_lamports(&self) -> ConfigResult<Option<u64>> {
        if self.init_lamports.is_some() && self.init_sol.is_some() {
            return Err(ConfigError::CannotSpecifyBothInitLamportAndInitSol);
        }
        Ok(match self.init_lamports {
            Some(lamports) => Some(lamports),
            None => self.init_sol.map(|sol| sol * LAMPORTS_PER_SOL),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedProgram {
    #[serde(
        deserialize_with = "pubkey_deserialize",
        serialize_with = "pubkey_serialize"
    )]
    pub id: Pubkey,
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
