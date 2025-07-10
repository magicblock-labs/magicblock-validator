use clap::Args;
use magicblock_config_macro::{clap_from_serde, clap_prefix};
use serde::{Deserialize, Serialize};

use crate::helpers::serde_defaults::bool_true;

// Default desired ledger size 100 GiB
pub const DEFAULT_LEDGER_SIZE_BYTES: u64 = 100 * 1024 * 1024 * 1024;

#[clap_prefix("ledger")]
#[clap_from_serde]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LedgerConfig {
    /// If a previous ledger is found it is removed before starting the validator
    /// This can be disabled by setting [Self::reset] to `false`.
    #[derive_env_var]
    #[clap_from_serde_skip]
    #[arg(help = "Whether to reset the ledger before starting the validator.")]
    #[serde(default = "bool_true")]
    pub reset: bool,
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

const fn default_ledger_size() -> u64 {
    DEFAULT_LEDGER_SIZE_BYTES
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            reset: bool_true(),
            path: Default::default(),
            size: DEFAULT_LEDGER_SIZE_BYTES,
        }
    }
}
