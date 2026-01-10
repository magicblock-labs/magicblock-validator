use std::{fs, path::Path};

use lmdb::{
    Cursor, Environment, EnvironmentFlags, Error as LmdbError, RoCursor,
    RoTransaction,
};
use solana_pubkey::Pubkey;

use super::{table::Table, Offset};
use crate::{index::Blocks, AccountsDbResult};

// Re-exporting LMDB constants that are missing from the safe wrapper crate.
// Values correspond to MDB_SET_RANGE and MDB_SET in lmdb.h
pub(super) const MDB_SET_RANGE_OP: u32 = 17;
pub(super) const MDB_SET_OP: u32 = 15;

const MAX_DBS: u32 = 4;

/// Configures and opens the LMDB environment.
///
/// Renamed from `lmdb_env` for clarity.
pub(super) fn create_lmdb_env(
    dir: &Path,
    map_size: usize,
) -> lmdb::Result<Environment> {
    // Flags optimization:
    // - NO_SYNC: Let OS manage flushing (we handle durability via snapshots/other means).
    // - WRITE_MAP: Use mmap for writes (avoids double buffering).
    // - NO_MEM_INIT: Don't zero-out memory (we overwrite anyway).
    // - NO_READAHEAD: Random access pattern optimization.
    let flags = EnvironmentFlags::NO_SYNC
        | EnvironmentFlags::WRITE_MAP
        | EnvironmentFlags::NO_MEM_INIT
        | EnvironmentFlags::NO_READAHEAD;

    let index_dir = dir.join("index");
    if !index_dir.exists() {
        fs::create_dir_all(&index_dir)
            .map_err(|e| LmdbError::Other(e.raw_os_error().unwrap_or(0)))?;
    }

    Environment::new()
        .set_map_size(map_size)
        .set_max_dbs(MAX_DBS)
        .set_flags(flags)
        .open_with_permissions(&index_dir, 0o644)
}

/// A self-contained cursor wrapper for efficient account lookups.
///
/// Holds both the transaction and the cursor to ensure safe lifetime management
/// while exposing a simple API.
pub struct AccountOffsetFinder<'env> {
    cursor: RoCursor<'env>,
    // Kept alive to support the cursor's lifetime.
    // Note: Field order matters for Drop! `cursor` depends on `_txn`, so `cursor` must be dropped first.
    // Rust drops fields in declaration order (top to bottom).
    _txn: RoTransaction<'env>,
}

impl<'env> AccountOffsetFinder<'env> {
    /// Creates a new finder instance.
    pub(super) fn new(
        table: &Table,
        txn: RoTransaction<'env>,
    ) -> AccountsDbResult<Self> {
        let cursor = table.cursor_ro(&txn)?;

        // SAFETY:
        // We are constructing a self-referential struct.
        // 1. `txn` is moved into the struct.
        // 2. `cursor` is created from `txn` but we must extend its lifetime to `'env`
        //    to store it alongside `txn`.
        //
        // This is safe because:
        // - LMDB cursors are valid as long as the transaction is open.
        // - We ensure `cursor` is dropped before `_txn` via struct field order.
        let cursor: RoCursor<'env> = unsafe { std::mem::transmute(cursor) };

        Ok(Self { cursor, _txn: txn })
    }

    /// Finds the storage offset for a given account public key.
    pub(crate) fn find(&self, pubkey: &Pubkey) -> Option<Offset> {
        let key_bytes = pubkey.as_ref();

        // MDB_SET_OP: Position at specified key
        self.cursor
            .get(Some(key_bytes), None, MDB_SET_OP)
            .ok()
            .map(|(_, val_bytes)| bytes!(#unpack, val_bytes, Offset, Blocks).0)
    }
}
