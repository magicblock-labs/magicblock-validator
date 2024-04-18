// -----------------
// AccessType
// -----------------
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessType {
    /// Primary (read/write) access; only one process can have Primary access.
    Primary,
    /// Primary (read/write) access with RocksDB automatic compaction disabled.
    #[allow(unused)]
    PrimaryForMaintenance,
    /// Secondary (read) access; multiple processes can have Secondary access.
    /// Additionally, Secondary access can be obtained while another process
    /// already has Primary access.
    #[allow(unused)]
    Secondary,
}

// -----------------
// BlockstoreOptions
// -----------------
pub struct BlockstoreOptions {
    // The access type of blockstore. Default: Primary
    pub access_type: AccessType,
    pub column_options: LedgerColumnOptions,
}

impl Default for BlockstoreOptions {
    /// The default options are the values used by [`Blockstore::open`].
    ///
    /// [`Blockstore::open`]: crate::blockstore::Blockstore::open
    fn default() -> Self {
        Self {
            access_type: AccessType::Primary,
            column_options: LedgerColumnOptions::default(),
        }
    }
}

// -----------------
// LedgerColumnOptions
// -----------------
/// Options for LedgerColumn.
/// Each field might also be used as a tag that supports group-by operation when
/// reporting metrics.
#[derive(Debug, Clone)]
pub struct LedgerColumnOptions {
    // Determine how to store both data and coding shreds. Default: RocksLevel.
    pub shred_storage_type: ShredStorageType,

    // Determine the way to compress column families which are eligible for
    // compression.
    pub compression_type: BlockstoreCompressionType,

    // Control how often RocksDB read/write performance samples are collected.
    // If the value is greater than 0, then RocksDB read/write perf sample
    // will be collected once for every `rocks_perf_sample_interval` ops.
    pub rocks_perf_sample_interval: usize,
}

impl Default for LedgerColumnOptions {
    fn default() -> Self {
        Self {
            shred_storage_type: ShredStorageType::RocksLevel,
            compression_type: BlockstoreCompressionType::default(),
            rocks_perf_sample_interval: 0,
        }
    }
}

impl LedgerColumnOptions {
    pub fn get_storage_type_string(&self) -> &'static str {
        match self.shred_storage_type {
            ShredStorageType::RocksLevel => "rocks_level",
        }
    }

    pub fn get_compression_type_string(&self) -> &'static str {
        match self.compression_type {
            BlockstoreCompressionType::None => "None",
            BlockstoreCompressionType::Snappy => "Snappy",
            BlockstoreCompressionType::Lz4 => "Lz4",
            BlockstoreCompressionType::Zlib => "Zlib",
        }
    }
}

// -----------------
// ShredStorageType
// -----------------
pub const BLOCKSTORE_DIRECTORY_ROCKS_LEVEL: &str = "rocksdb";

#[derive(Debug, Default, Clone)]
pub enum ShredStorageType {
    // Stores shreds under RocksDB's default compaction (level).
    #[default]
    RocksLevel,
    // NOTE: we aren't supporting experimental BlockstoreRocksFifoOptions
}

impl ShredStorageType {
    /// The directory under `ledger_path` to the underlying blockstore.
    pub fn blockstore_directory(&self) -> &str {
        match self {
            ShredStorageType::RocksLevel => BLOCKSTORE_DIRECTORY_ROCKS_LEVEL,
        }
    }
}

// -----------------
// BlockstoreCompressionType
// -----------------
#[derive(Debug, Default, Clone)]
pub enum BlockstoreCompressionType {
    #[default]
    None,
    Snappy,
    Lz4,
    Zlib,
}
