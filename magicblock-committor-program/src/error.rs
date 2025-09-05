use solana_program::{msg, program_error::ProgramError};
use thiserror::Error;

use crate::state::chunks::ChunksError;

pub type CommittorResult<T> = std::result::Result<T, CommittorError>;

#[derive(Error, Debug, Clone)]
pub enum CommittorError {
    #[error("Unable to serialize change set: {0}")]
    UnableToSerializeChangeSet(String),

    #[error("Pubkey error")]
    PubkeyError(#[from] solana_pubkey::PubkeyError),

    #[error("Offset ({0}) must be multiple of chunk size ({1})")]
    OffsetMustBeMultipleOfChunkSize(usize, u16),

    #[error("Chunk of size {0} cannot be stored at offset {1} in buffer of size ({2})")]
    OffsetChunkOutOfRange(usize, u32, usize),

    #[error("Out of bound access to chunks")]
    OutOfBoundsError,
}

impl From<ChunksError> for CommittorError {
    fn from(value: ChunksError) -> Self {
        match value {
            ChunksError::OutOfBoundsError => CommittorError::OutOfBoundsError,
            ChunksError::InvalidOffsetError(offset, chunk_size) => {
                CommittorError::OffsetMustBeMultipleOfChunkSize(
                    offset, chunk_size,
                )
            }
        }
    }
}

impl From<CommittorError> for ProgramError {
    fn from(e: CommittorError) -> Self {
        msg!("Error: {:?}", e);
        use CommittorError::*;
        let n = match e {
            UnableToSerializeChangeSet(_) => 0x69000,
            PubkeyError(_) => 0x69001,
            OffsetMustBeMultipleOfChunkSize(_, _) => 0x69002,
            OffsetChunkOutOfRange(_, _, _) => 0x69003,
            OutOfBoundsError => 0x69004,
        };
        ProgramError::Custom(n)
    }
}
