use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// path to root directory where database files are stored
    pub directory: PathBuf,
    /// size of the main storage, we have to preallocate in advance
    pub db_size: usize,
    /// minimal indivisible unit of addressing in main storage
    /// offsets are calculated in terms of blocks
    pub block_size: BlockSize,
    /// size of index file, we have to preallocate, can be 1% of main storage size
    pub index_map_size: usize,
    /// max number of snapshots to keep around
    pub max_snapshots: u16,
    /// how frequently (slot-wise) we should take snapshots
    pub snapshot_frequency: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
#[repr(u32)]
pub enum BlockSize {
    Block128 = 128,
    Block256 = 256,
    Block512 = 512,
}
