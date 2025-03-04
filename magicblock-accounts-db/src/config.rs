use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct AdbConfig {
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

pub const TEST_SNAPSHOT_FREQUENCY: u64 = 50;
impl Default for AdbConfig {
    fn default() -> Self {
        Self::temp_for_tests(TEST_SNAPSHOT_FREQUENCY)
    }
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize,
)]
#[serde(rename_all = "kebab-case")]
#[repr(u32)]
pub enum BlockSize {
    Block128 = 128,
    #[default]
    Block256 = 256,
    Block512 = 512,
}

impl AdbConfig {
    pub fn temp_for_tests(snapshot_frequency: u64) -> Self {
        use std::fs;
        const DB_SIZE: usize = 100 * 1024 * 1024;
        const BLOCK_SIZE: BlockSize = BlockSize::Block256;
        const INDEX_MAP_SIZE: usize = 1024 * 1024 * 10;
        const MAX_SNAPSHOTS: u16 = 32;

        let (_, nanos): (u64, u32) =
            unsafe { std::mem::transmute(std::time::Instant::now()) };
        // indexing, so that each test will run with its own adb
        let tempdir = tempfile::tempdir()
            .expect("failed to create a temporary directory");
        let directory: PathBuf =
            tempdir.into_path().join(format!("adb-test{nanos}/adb"));
        let parent = directory.parent().expect("must have a parent"); // infallible
        let _ = fs::remove_dir_all(parent);

        Self {
            directory,
            block_size: BLOCK_SIZE,
            db_size: DB_SIZE,
            max_snapshots: MAX_SNAPSHOTS,
            snapshot_frequency,
            index_map_size: INDEX_MAP_SIZE,
        }
    }
}
