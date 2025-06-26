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
