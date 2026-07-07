use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for the ledger database and block production.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct LedgerConfig {
    /// Target duration for a single block slot.
    /// Default: 50ms.
    #[serde(with = "humantime")]
    pub block_time: Duration,

    /// The number of slots that must elapse before
    /// the accountsdb snapshot/checksum is taken
    pub superblock_size: u64,

    /// If true, the existing ledger database will be wiped on startup.
    /// Useful for ephemeral or testing environments.
    pub reset: bool,

    /// Whether to verify the validator's keypair against the ledger's identity
    /// to prevent accidental startup with the wrong key.
    pub verify_keypair: bool,

    /// Upper hard threshold for the max size of the ledger,
    /// ledger truncation logic kicks in, when the disk space
    /// used by the ledger approaches this number.
    pub size: u64,

    /// Capacity in bytes of the RocksDB block cache shared by all ledger
    /// columns. This is the only read caching layer since the ledger
    /// bypasses the OS page cache, so production nodes should set 16 GB
    /// or more. Default: 512 MB.
    pub block_cache_size: u64,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            block_time: Duration::from_millis(
                consts::DEFAULT_LEDGER_BLOCK_TIME_MS,
            ),

            superblock_size: consts::DEFAULT_SUPERBLOCK_SIZE,
            reset: false,
            verify_keypair: true,
            size: consts::DEFAULT_LEDGER_SIZE,
            block_cache_size: consts::DEFAULT_LEDGER_BLOCK_CACHE_SIZE,
        }
    }
}

impl LedgerConfig {
    /// Returns configured block time in milliseconds
    pub fn block_time_ms(&self) -> u64 {
        self.block_time.as_millis() as u64
    }
}
