use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    /// If a previous ledger is found it is removed before starting the validator
    /// This can be disabled by setting [Self::reset] to `false`.
    #[serde(default = "default_reset")]
    pub reset: bool,
    // The file system path onto which the ledger should be written at
    // If left empty it will be auto-generated to a temporary folder
    #[serde(default = "default_path")]
    pub path: Option<String>,
}

fn default_reset() -> bool {
    true
}

fn default_path() -> Option<String> {
    None
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            reset: default_reset(),
            path: default_path(),
        }
    }
}
