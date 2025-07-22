use clap::{Args, ValueEnum};
use magicblock_config_macro::{clap_from_serde, clap_prefix, Mergeable};
use serde::{Deserialize, Serialize};
use strum::Display;

#[clap_prefix("db")]
#[clap_from_serde]
#[derive(
    Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Args, Mergeable,
)]
#[serde(rename_all = "kebab-case")]
pub struct AccountsDbConfig {
    /// size of the main storage, we have to preallocate in advance
    #[arg(
        help = "The size of the main storage, we have to preallocate in advance."
    )]
    #[serde(default = "default_db_size")]
    pub db_size: usize,
    /// minimal indivisible unit of addressing in main storage
    /// offsets are calculated in terms of blocks
    #[arg(
        help = "The minimal indivisible unit of addressing in main storage."
    )]
    #[serde(default)]
    pub block_size: BlockSize,
    /// size of index file, we have to preallocate, can be 1% of main storage size
    #[arg(
        help = "The size of the DB index file, we have to preallocate, can be 1% of main storage size."
    )]
    #[serde(default = "default_index_map_size")]
    pub index_map_size: usize,
    #[arg(help = "The max number of DB snapshots to keep around.")]
    /// max number of snapshots to keep around
    #[serde(default = "default_max_snapshots")]
    pub max_snapshots: u16,
    /// how frequently (slot-wise) we should take snapshots
    #[arg(help = "How frequently (slot-wise) we should take DB snapshots.")]
    #[serde(default = "default_snapshot_frequency")]
    pub snapshot_frequency: u64,
}

pub const TEST_SNAPSHOT_FREQUENCY: u64 = 50;
impl Default for AccountsDbConfig {
    fn default() -> Self {
        Self {
            block_size: BlockSize::Block256,
            db_size: default_db_size(),
            index_map_size: default_index_map_size(),
            max_snapshots: default_max_snapshots(),
            snapshot_frequency: TEST_SNAPSHOT_FREQUENCY,
        }
    }
}

#[derive(
    Debug,
    Default,
    Display,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
#[repr(u32)]
pub enum BlockSize {
    Block128 = 128,
    #[default]
    Block256 = 256,
    Block512 = 512,
}

const fn default_db_size() -> usize {
    100 * 1024 * 1024
}

const fn default_index_map_size() -> usize {
    1024 * 1024 * 10
}

const fn default_max_snapshots() -> u16 {
    32
}

const fn default_snapshot_frequency() -> u64 {
    TEST_SNAPSHOT_FREQUENCY
}

impl AccountsDbConfig {
    pub fn temp_for_tests(snapshot_frequency: u64) -> Self {
        const DB_SIZE: usize = default_db_size();
        const BLOCK_SIZE: BlockSize = BlockSize::Block256;
        const INDEX_MAP_SIZE: usize = default_index_map_size();
        const MAX_SNAPSHOTS: u16 = default_max_snapshots();

        Self {
            block_size: BLOCK_SIZE,
            db_size: DB_SIZE,
            max_snapshots: MAX_SNAPSHOTS,
            snapshot_frequency,
            index_map_size: INDEX_MAP_SIZE,
        }
    }
}
