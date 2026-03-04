use std::{
    error::Error,
    sync::{Arc, RwLock},
};

use lazy_static::lazy_static;
use magicblock_core::{intent::CommittedAccount, traits::MagicSys};
use solana_instruction::error::InstructionError;

lazy_static! {
    static ref MAGIC_SYS: RwLock<Option<Arc<dyn MagicSys>>> = RwLock::new(None);
}

const MAGIC_SYS_POISONED_MSG: &str = "MAGIC_SYS poisoned";
const MAGIC_SYS_UNSET_MSG: &str = "MagicSys needs to be set on startup";

pub fn init_magic_sys<T: MagicSys>(magic_sys: Arc<T>) {
    MAGIC_SYS
        .write()
        .expect(MAGIC_SYS_POISONED_MSG)
        .replace(magic_sys);
}

pub(crate) fn load_data(id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    MAGIC_SYS
        .read()
        .expect(MAGIC_SYS_POISONED_MSG)
        .as_ref()
        .ok_or(MAGIC_SYS_UNSET_MSG)?
        .load(id)
}

pub(crate) fn persist_data(
    id: u64,
    data: Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    MAGIC_SYS
        .read()
        .expect(MAGIC_SYS_POISONED_MSG)
        .as_ref()
        .ok_or(MAGIC_SYS_UNSET_MSG)?
        .persist(id, data)
}

pub(crate) fn validate_commits(
    commits: &[CommittedAccount],
) -> Result<(), InstructionError> {
    MAGIC_SYS
        .read()
        .expect(MAGIC_SYS_POISONED_MSG)
        .as_ref()
        .ok_or(InstructionError::UninitializedAccount)?
        .validate_commits(commits)
}
