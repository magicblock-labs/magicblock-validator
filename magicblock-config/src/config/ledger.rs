use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for the ledger database and block production.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct LedgerConfig {
    /// If true, the existing ledger database will be wiped on startup.
    /// Useful for ephemeral or testing environments.
    pub reset: bool,

    /// Whether to verify the validator's keypair against the ledger's identity
    /// to prevent accidental startup with the wrong key.
    pub verify_keypair: bool,

    /// Capacity in bytes of the RocksDB block cache shared by all ledger
    /// columns. This is the only read caching layer since the ledger
    /// bypasses the OS page cache, so production nodes should set 16 GB
    /// or more. Default: 512 MB.
    pub block_cache_size: u64,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            reset: false,
            verify_keypair: true,
            block_cache_size: consts::DEFAULT_LEDGER_BLOCK_CACHE_SIZE,
        }
    }
}
