use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::SerdePubkey;

/// Describes an external program to load at startup.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct LoadableProgram {
    /// The Program ID (Pubkey) to assign to the loaded program.
    pub id: SerdePubkey,

    /// File system path to the compiled Solana program (.so file).
    pub path: PathBuf,
}
