use std::{fs, path::Path};

use lmdb::{Environment, EnvironmentFlags};

// Below is the list of LMDB cursor operation consts, which were copy
// pasted since they are not exposed in the public API of LMDB
// See https://github.com/mozilla/lmdb-rs/blob/946167603dd6806f3733e18f01a89cee21888468/lmdb-sys/src/bindings.rs#L158

#[doc = "Position at first key greater than or equal to specified key."]
pub(super) const MDB_SET_RANGE_OP: u32 = 17;
#[doc = "Position at specified key"]
pub(super) const MDB_SET_OP: u32 = 15;
#[doc = "Position at first key/data item"]
pub(super) const MDB_FIRST_OP: u32 = 0;
#[doc = "Position at next data item"]
pub(super) const MDB_NEXT_OP: u32 = 8;
#[doc = "Position at next data item of current key. Only for #MDB_DUPSORT"]
pub(super) const MDB_NEXT_DUP_OP: u32 = 9;
#[doc = "Return key/data at current cursor position"]
pub(super) const MDB_GET_CURRENT_OP: u32 = 4;
#[doc = "Position at key/data pair. Only for #MDB_DUPSORT"]
pub(super) const MDB_GET_BOTH_OP: u32 = 2;

pub(super) fn lmdb_env(
    name: &str,
    dir: &Path,
    size: usize,
    maxdb: u32,
) -> lmdb::Result<Environment> {
    let lmdb_env_flags: EnvironmentFlags =
        // allows to manually trigger flush syncs, but OS initiated flushes are somewhat beyond our control
        EnvironmentFlags::NO_SYNC
        // don't bother with copy on write and mutate the memory
        // directly, saves CPU cycles and memory access
        | EnvironmentFlags::WRITE_MAP
        // we never read uninit memory, so there's no point in paying for meminit
        | EnvironmentFlags::NO_MEM_INIT;

    let path = dir.join(name);
    let _ = fs::create_dir_all(&path);
    Environment::new()
        .set_map_size(size)
        .set_max_dbs(maxdb)
        .set_flags(lmdb_env_flags)
        .open_with_permissions(&path, 0o644)
}

/// Utility type to enforce big endian representation of u32. This is useful when u32
/// is used as a key in lmdb and we need an ascending ordering on byte representation
pub(super) struct BigEndianU32([u8; 4]);

impl BigEndianU32 {
    pub(super) fn new(val: u32) -> Self {
        Self(val.to_be_bytes())
    }
}

impl AsRef<[u8]> for BigEndianU32 {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
