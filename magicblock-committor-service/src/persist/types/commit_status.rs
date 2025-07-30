use std::fmt;

use solana_sdk::signature::Signature;

use crate::persist::error::CommitPersistError;

/// The status of a committed account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitStatus {
    /// We sent the request to commit this account, but haven't received a result yet.
    Pending,
    /// No part of the commit pipeline succeeded.
    /// The commit for this account needs to be restarted from scratch.
    Failed,
    /// The buffer and chunks account were initialized, but could either not
    /// be retrieved or deserialized. It is recommended to fully re-initialize
    /// them on retry.
    BufferAndChunkPartiallyInitialized,
    /// The buffer and chunks accounts were initialized and could be
    /// deserialized, however we did not complete writing to them
    /// We can reuse them on retry, but need to rewrite all chunks.
    BufferAndChunkInitialized,
    /// The buffer and chunks accounts were initialized and all data was
    /// written to them (for data accounts).
    /// This means on retry we can skip that step and just try to process
    /// these buffers to complete the commit.
    BufferAndChunkFullyInitialized,
    /// The commit is part of a bundle that contains too many commits to be included
    /// in a single transaction. Thus we cannot commit any of them.
    PartOfTooLargeBundleToProcess,
    /// The commit was properly initialized and added to a chunk of instructions to process
    /// commits via a transaction. For large commits the buffer and chunk accounts were properly
    /// prepared and haven't been closed.
    FailedProcess(Option<CommitStatusSignatures>),
    /// The commit was properly processed but the requested finalize transaction failed.
    FailedFinalize(CommitStatusSignatures),
    /// The commit was successfully processed and finalized.
    Succeeded(CommitStatusSignatures),
}

impl fmt::Display for CommitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitStatus::Pending => write!(f, "Pending"),
            CommitStatus::Failed => {
                write!(f, "Failed")
            }
            CommitStatus::BufferAndChunkPartiallyInitialized => {
                write!(f, "BufferAndChunkPartiallyInitialized")
            }
            CommitStatus::BufferAndChunkInitialized => {
                write!(f, "BufferAndChunkInitialized")
            }
            CommitStatus::BufferAndChunkFullyInitialized => {
                write!(f, "BufferAndChunkFullyInitialized")
            }
            CommitStatus::PartOfTooLargeBundleToProcess => {
                write!(f, "PartOfTooLargeBundleToProcess")
            }
            CommitStatus::FailedProcess(sigs) => {
                write!(f, "FailedProcess({:?})", sigs)
            }
            CommitStatus::FailedFinalize(sigs) => {
                write!(f, "FailedFinalize({:?})", sigs)
            }
            CommitStatus::Succeeded(sigs) => {
                write!(f, "Succeeded({:?})", sigs)
            }
        }
    }
}

impl TryFrom<(&str, Option<CommitStatusSignatures>)> for CommitStatus {
    type Error = CommitPersistError;

    fn try_from(
        (status, sigs): (&str, Option<CommitStatusSignatures>),
    ) -> Result<Self, Self::Error> {
        let get_sigs = || {
            if let Some(sigs) = sigs.clone() {
                Ok(sigs)
            } else {
                return Err(CommitPersistError::CommitStatusNeedsSignatures(
                    status.to_string(),
                ));
            }
        };

        use CommitStatus::*;
        match status {
            "Pending" => Ok(Pending),
            "Failed" => Ok(Failed),
            "BufferAndChunkPartiallyInitialized" => {
                Ok(BufferAndChunkPartiallyInitialized)
            }
            "BufferAndChunkInitialized" => Ok(BufferAndChunkInitialized),
            "BufferAndChunkFullyInitialized" => {
                Ok(BufferAndChunkFullyInitialized)
            }
            "PartOfTooLargeBundleToProcess" => {
                Ok(PartOfTooLargeBundleToProcess)
            }
            "FailedProcess" => Ok(FailedProcess(sigs)),
            "FailedFinalize" => Ok(FailedFinalize(get_sigs()?)),
            "Succeeded" => Ok(Succeeded(get_sigs()?)),
            _ => {
                Err(CommitPersistError::InvalidCommitStatus(status.to_string()))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitStatusSignatures {
    /// The signature of the transaction processing the commit
    pub process_signature: Signature,
    /// The signature of the transaction finalizing the commit.
    /// If the account was not finalized or it failed then this is `None`.
    /// If the finalize instruction was part of the process transaction then
    /// this signature is the same as [Self::process_signature].
    pub finalize_signature: Option<Signature>,
}

impl CommitStatus {
    pub fn as_str(&self) -> &str {
        use CommitStatus::*;
        match self {
            Pending => "Pending",
            Failed => "Failed",
            BufferAndChunkPartiallyInitialized => {
                "BufferAndChunkPartiallyInitialized"
            }
            BufferAndChunkInitialized => "BufferAndChunkInitialized",
            BufferAndChunkFullyInitialized => "BufferAndChunkFullyInitialized",
            PartOfTooLargeBundleToProcess => "PartOfTooLargeBundleToProcess",
            FailedProcess(_) => "FailedProcess",
            FailedFinalize(_) => "FailedFinalize",
            Succeeded(_) => "Succeeded",
        }
    }

    pub fn signatures(&self) -> Option<CommitStatusSignatures> {
        use CommitStatus::*;
        match self {
            FailedProcess(sigs) => sigs.as_ref().cloned(),
            FailedFinalize(sigs) => Some(sigs.clone()),
            Succeeded(sigs) => Some(sigs.clone()),
            _ => None,
        }
    }

    /// The commit fully succeeded and no retry is necessary.
    pub fn is_complete(&self) -> bool {
        use CommitStatus::*;
        matches!(self, Succeeded(_))
    }

    pub fn all_completed(stages: &[Self]) -> bool {
        stages.iter().all(Self::is_complete)
    }
}
