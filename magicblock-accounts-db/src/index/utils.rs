use std::{fs, path::Path};

use lmdb::{Cursor, Environment, EnvironmentFlags, RoCursor, RoTransaction};
use solana_pubkey::Pubkey;

use crate::{index::Blocks, AdbResult};

use super::{table::Table, Offset};

// Below is the list of LMDB cursor operation consts, which were copy
// pasted since they are not exposed in the public API of LMDB
// See https://github.com/mozilla/lmdb-rs/blob/946167603dd6806f3733e18f01a89cee21888468/lmdb-sys/src/bindings.rs#L158

#[doc = "Position at first key greater than or equal to specified key."]
pub(super) const MDB_SET_RANGE_OP: u32 = 17;
#[doc = "Position at specified key"]
pub(super) const MDB_SET_OP: u32 = 15;

const TABLES_COUNT: u32 = 4;

pub(super) fn lmdb_env(dir: &Path, size: usize) -> lmdb::Result<Environment> {
    let lmdb_env_flags: EnvironmentFlags =
        // allows to manually trigger flush syncs, but OS initiated flushes are somewhat beyond our control
        EnvironmentFlags::NO_SYNC
        // don't bother with copy on write and mutate the memory
        // directly, saves CPU cycles and memory access
        | EnvironmentFlags::WRITE_MAP
        // we never read uninit memory, so there's no point in paying for meminit
        | EnvironmentFlags::NO_MEM_INIT
        // accounts' access is pretty much random, so read ahead might be doing unecessary work
        | EnvironmentFlags::NO_READAHEAD;

    let path = dir.join("index");
    let _ = fs::create_dir_all(&path);
    Environment::new()
        .set_map_size(size)
        .set_max_dbs(TABLES_COUNT)
        .set_flags(lmdb_env_flags)
        .open_with_permissions(&path, 0o644)
}

/// A wrapper around a cursor on the accounts table
pub struct AccountOffsetFinder<'env> {
    cursor: RoCursor<'env>,
    _txn: RoTransaction<'env>,
}

impl<'env> AccountOffsetFinder<'env> {
    /// Set up a new cursor
    pub(super) fn new(
        table: &Table,
        txn: RoTransaction<'env>,
    ) -> AdbResult<Self> {
        let cursor = table.cursor_ro(&txn)?;
        // SAFETY:
        // nasty/neat trick for lifetime erasure, but we are upholding
        // the rust's ownership contracts by keeping txn around as well
        let cursor: RoCursor = unsafe { std::mem::transmute(cursor) };
        Ok(Self { cursor, _txn: txn })
    }

    /// Find a storage offset for the given account
    pub(crate) fn find(&self, pubkey: &Pubkey) -> Option<Offset> {
        self.cursor
            .get(Some(pubkey.as_ref()), None, MDB_SET_OP)
            .ok()
            .map(|(_, v)| bytes!(#unpack, v, Offset, Blocks).0)
    }
}
