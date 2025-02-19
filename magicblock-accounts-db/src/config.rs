use std::{path::PathBuf, sync::atomic::AtomicUsize};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
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
        const DB_SIZE: usize = 10 * 1024 * 1024;
        const BLOCK_SIZE: BlockSize = BlockSize::Block256;
        const INDEX_MAP_SIZE: usize = 1024 * 1024;
        const MAX_SNAPSHOTS: u16 = 4;

        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let i = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // indexing, so that each test will run with its own adb
        let directory: PathBuf =
            format!("/tmp/adb-test{i}/adb").parse().unwrap();
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory)
            .expect("expected to create temporary adb directory");

        Self {
            directory: directory.clone(),
            block_size: BLOCK_SIZE,
            db_size: DB_SIZE,
            max_snapshots: MAX_SNAPSHOTS,
            snapshot_frequency,
            index_map_size: INDEX_MAP_SIZE,
        }
    }
}
