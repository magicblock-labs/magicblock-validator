use solana_program::msg;
use solana_program::program_error::ProgramError;
use thiserror::Error;

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
        };
        ProgramError::Custom(n)
    }
}
