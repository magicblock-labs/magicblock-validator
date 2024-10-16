use serde::{Deserialize, Serialize};

use crate::helpers::serde_defaults::bool_true;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerConfig {
    /// If a previous ledger is found it is removed before starting the validator
    /// This can be disabled by setting [Self::reset] to `false`.
    #[serde(default = "bool_true")]
    pub reset: bool,
    // The file system path onto which the ledger should be written at
    // If left empty it will be auto-generated to a temporary folder
    #[serde(default)]
    pub path: Option<String>,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            reset: bool_true(),
            path: Default::default(),
        }
    }
}
