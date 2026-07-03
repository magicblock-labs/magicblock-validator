use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use lazy_static::lazy_static;
use magicblock_core::{
    intent::{schedule::MagicIntentBundle, CommittedAccount},
    traits::MagicSys,
};
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;

/// Maximum number of times an account may be committed before it must be
/// undelegated. A plain commit at or beyond this limit fails with [`COMMIT_LIMIT_ERR`].
pub const COMMIT_LIMIT: u64 = 10;
/// [`InstructionError::Custom`] code returned when a commit is attempted on an
/// account that has reached [`COMMIT_LIMIT`].
pub const COMMIT_LIMIT_ERR: u32 = 0xA000_0000;
pub(crate) const MISSING_COMMIT_NONCE_ERR: u32 = 0xA000_0001;
/// [`InstructionError::Custom`] code returned when an intent could never fit
/// on the base layer, no matter how it gets optimized at execution time.
pub const INTENT_TOO_LARGE_ERR: u32 = 0xA000_0002;

lazy_static! {
    static ref MAGIC_SYS: RwLock<Option<Arc<dyn MagicSys>>> = RwLock::new(None);
}

const MAGIC_SYS_POISONED_MSG: &str = "MAGIC_SYS poisoned";

pub fn init_magic_sys<T: MagicSys>(magic_sys: Arc<T>) {
    MAGIC_SYS
        .write()
        .expect(MAGIC_SYS_POISONED_MSG)
        .replace(magic_sys);
}

pub(crate) fn fetch_current_commit_nonces(
    commits: &[CommittedAccount],
) -> Result<HashMap<Pubkey, u64>, InstructionError> {
    MAGIC_SYS
        .read()
        .expect(MAGIC_SYS_POISONED_MSG)
        .as_ref()
        .ok_or(InstructionError::UninitializedAccount)?
        .fetch_current_commit_nonces(commits)
}

pub(crate) fn validate_intent_size(
    intent: &MagicIntentBundle,
) -> Result<(), InstructionError> {
    MAGIC_SYS
        .read()
        .expect(MAGIC_SYS_POISONED_MSG)
        .as_ref()
        .ok_or(InstructionError::UninitializedAccount)?
        .validate_intent_size(intent)
}
