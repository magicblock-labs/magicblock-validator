use std::error::Error;

use serde::{Deserialize, Serialize};
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use url::Url;

// -----------------
// AccountsConfig
// -----------------
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AccountsConfig {
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub clone: CloneStrategy,
    #[serde(default)]
    pub commit: CommitStrategy,
    #[serde(default = "default_create")]
    pub create: bool,
    #[serde(default)]
    pub payer: Payer,
}

fn default_create() -> bool {
    true
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            remote: Default::default(),
            clone: Default::default(),
            payer: Default::default(),
            create: true,
            commit: Default::default(),
        }
    }
}

// -----------------
// RemoteConfig
// -----------------
#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteConfig {
    #[default]
    Devnet,
    #[serde(alias = "mainnet-beta")]
    Mainnet,
    Testnet,
    #[serde(alias = "local")]
    #[serde(alias = "localhost")]
    Development,
    #[serde(
        untagged,
        deserialize_with = "deserialize_url",
        serialize_with = "serialize_url"
    )]
    Custom(Url),
}

fn deserialize_url<'de, D>(deserializer: D) -> Result<Url, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Url::parse(&s).map_err(|err| {
        // The error returned here by serde is a bit unhelpful so we help out
        // by logging a bit more information.
        eprintln!("RemoteConfig encountered invalid URL ({}).", err);
        serde::de::Error::custom(err)
    })
}

fn serialize_url<S>(url: &Url, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(url.as_ref())
}

// -----------------
// CloneStrategy
// -----------------
#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct CloneStrategy {
    #[serde(default)]
    pub readonly: ReadonlyMode,
    #[serde(default)]
    pub writable: WritableMode,
}

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadonlyMode {
    All,
    #[default]
    #[serde(alias = "program")]
    Programs,
    None,
}

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WritableMode {
    All,
    Delegated,
    #[default]
    None,
}

// -----------------
// CommitStrategy
// -----------------
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct CommitStrategy {
    #[serde(default = "default_frequency_millis")]
    pub frequency_millis: u64,
}

fn default_frequency_millis() -> u64 {
    500
}

impl Default for CommitStrategy {
    fn default() -> Self {
        Self {
            frequency_millis: default_frequency_millis(),
        }
    }
}

// -----------------
// Payer
// -----------------
#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Payer {
    /// The payer init balance in lamports.
    /// Read it via [Self::try_init_lamports].
    init_lamports: Option<u64>,
    /// The payer init balance in SOL.
    /// Read it via [Self::try_init_lamports].
    init_sol: Option<u64>,
}

impl Payer {
    pub fn new(init_lamports: Option<u64>, init_sol: Option<u64>) -> Self {
        Self {
            init_lamports,
            init_sol,
        }
    }
    pub fn try_init_lamports(&self) -> Result<Option<u64>, Box<dyn Error>> {
        if self.init_lamports.is_some() && self.init_sol.is_some() {
            return Err("Cannot specify both init_lamports and init_sol".into());
        }
        Ok(match self.init_lamports {
            Some(lamports) => Some(lamports),
            None => self.init_sol.map(|sol| sol * LAMPORTS_PER_SOL),
        })
    }
}
