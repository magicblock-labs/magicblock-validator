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
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            block_time: Duration::from_millis(
                consts::DEFAULT_LEDGER_BLOCK_TIME_MS,
            ),
            reset: false,
            verify_keypair: true,
            size: consts::DEFAULT_LEDGER_SIZE,
        }
    }
}

impl LedgerConfig {
    /// Returns configured block time in milliseconds
    pub fn block_time_ms(&self) -> u64 {
        self.block_time.as_millis() as u64
    }
}
