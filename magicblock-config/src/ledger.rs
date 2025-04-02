use serde::{Deserialize, Serialize};

use crate::helpers::serde_defaults::bool_true;

// Default desired ledger size 100 GiB
pub const DEFAULT_DESIRED_SIZE: u64 = 100 * 1024 * 1024 * 1024;
// Lower bound on ledger size is 50 GiB
const MIN_DESIRED_SIZE: u64 = 50 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LedgerConfig {
    /// If a previous ledger is found it is removed before starting the validator
    /// This can be disabled by setting [Self::reset] to `false`.
    #[serde(default = "bool_true")]
    pub reset: bool,
    // The file system path onto which the ledger should be written at
    // If left empty it will be auto-generated to a temporary folder
    #[serde(default)]
    pub path: Option<String>,
    // The size under which it's desired to keep ledger in bytes.
    #[serde(default = "default_desired_size")]
    #[serde(deserialize_with = "validate_desired_size_deserializer")]
    pub desired_size: u64,
}

const fn default_desired_size() -> u64 {
    DEFAULT_DESIRED_SIZE
}

fn validate_desired_size_deserializer<'de, D>(
    deserializer: D,
) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if value < MIN_DESIRED_SIZE {
        Err(serde::de::Error::custom(format!(
            "desired_size must be at least {} bytes",
            MIN_DESIRED_SIZE
        )))
    } else {
        Ok(value)
    }
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            reset: bool_true(),
            path: Default::default(),
            desired_size: DEFAULT_DESIRED_SIZE,
        }
    }
}
