use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::time::Duration;

/// Configuration for the ledger database and block production.
#[serde_as]
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LedgerConfig {
    /// Target duration for a single block slot.
    /// Default: 400ms.
    #[serde(with = "humantime")]
    pub block_time: Duration,

    /// If true, the existing ledger database will be wiped on startup.
    /// Useful for ephemeral or testing environments.
    pub reset: bool,

    /// Whether to verify the validator's keypair against the ledger's identity
    /// to prevent accidental startup with the wrong key.
    pub verify_keypair: bool,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            block_time: Duration::from_millis(400),
            reset: false,
            verify_keypair: true,
        }
    }
}
