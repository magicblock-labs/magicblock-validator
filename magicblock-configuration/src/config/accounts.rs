use serde::{Deserialize, Serialize};

/// Configuration for the accounts database performance and storage.
#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AccountsDbConfig {
    /// Total size (in bytes) allocated for the accounts database file.
    /// Default: 100 MB.
    pub database_size: usize,

    /// The block size used for storage allocation within the accounts DB.
    pub block_size: BlockSize,

    /// Size (in bytes) allocated for the accounts index.
    /// Default: 1 MB.
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
            database_size: 100 * 1024 * 1024,
            index_size: 1024 * 1024,
            max_snapshots: 4,
            snapshot_frequency: 1024,
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
