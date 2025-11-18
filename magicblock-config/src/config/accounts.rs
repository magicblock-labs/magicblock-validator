use serde::{Deserialize, Serialize};

use crate::consts;

/// Configuration for the accounts database performance and storage.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case", deny_unknown_fields, default)]
pub struct AccountsDbConfig {
    /// Total size (in bytes) allocated for the accounts database file.
    pub database_size: usize,

    /// The block size used for storage allocation within the accounts DB.
    pub block_size: BlockSize,

    /// Size (in bytes) allocated for the accounts index.
    pub index_size: usize,

    /// Maximum number of historical snapshots to retain.
    pub max_snapshots: u16,

    /// Number of slots between generating a new snapshot.
    pub snapshot_frequency: u64,

    /// If true, wipes the accounts database on startup.
    pub reset: bool,
}

impl Default for AccountsDbConfig {
    fn default() -> Self {
        Self {
            block_size: BlockSize::Block256,
            database_size: consts::DEFAULT_ACCOUNTS_DB_SIZE,
            index_size: consts::DEFAULT_ACCOUNTS_INDEX_SIZE,
            max_snapshots: consts::DEFAULT_ACCOUNTS_MAX_SNAPSHOTS,
            snapshot_frequency: consts::DEFAULT_ACCOUNTS_SNAPSHOT_FREQUENCY,
            reset: false,
        }
    }
}

/// Allocation block size for the accounts DB.
#[derive(Deserialize, Serialize, Debug, Default, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum BlockSize {
    Block128 = 128,
    #[default]
    Block256 = 256,
    Block512 = 512,
}
