use serde::{Deserialize, Serialize};
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
