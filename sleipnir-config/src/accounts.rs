use serde::{Deserialize, Serialize};

// -----------------
// AccountsConfig
// -----------------
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AccountsConfig {
    #[serde(default)]
    pub remote: RemoteConfig,
    #[serde(default)]
    pub clone: CloneStrategy,
    #[serde(default = "default_create")]
    pub create: bool,
}

fn default_create() -> bool {
    true
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            remote: Default::default(),
            clone: Default::default(),
            create: true,
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
    Custom(String),
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
