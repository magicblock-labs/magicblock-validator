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

pub struct BlockstoreOptions {
    // The access type of blockstore. Default: Primary
    pub access_type: AccessType,
}

impl Default for BlockstoreOptions {
    /// The default options are the values used by [`Blockstore::open`].
    ///
    /// [`Blockstore::open`]: crate::blockstore::Blockstore::open
    fn default() -> Self {
        Self {
            access_type: AccessType::Primary,
        }
    }
}
