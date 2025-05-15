use std::fmt;

use solana_sdk::signature::Signature;

use super::commit_strategy::CommitStrategy;
use crate::persist::error::CommitPersistError;

/// The status of a committed account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitStatus {
    /// We sent the request to commit this account, but haven't received a result yet.
    Pending,
    /// No part of the commit pipeline succeeded.
    /// The commit for this account needs to be restarted from scratch.
    Failed(u64),
    /// The buffer and chunks account were initialized, but could either not
    /// be retrieved or deserialized. It is recommended to fully re-initialize
    /// them on retry.
    BufferAndChunkPartiallyInitialized(u64),
    /// The buffer and chunks accounts were initialized and could be
    /// deserialized, however we did not complete writing to them
    /// We can reuse them on retry, but need to rewrite all chunks.
    BufferAndChunkInitialized(u64),
    /// The buffer and chunks accounts were initialized and all data was
    /// written to them (for data accounts).
    /// This means on retry we can skip that step and just try to process
    /// these buffers to complete the commit.
    BufferAndChunkFullyInitialized(u64),
    /// The commit is part of a bundle that contains too many commits to be included
    /// in a single transaction. Thus we cannot commit any of them.
    PartOfTooLargeBundleToProcess(u64),
    /// The commmit was properly initialized and added to a chunk of instructions to process
    /// commits via a transaction. For large commits the buffer and chunk accounts were properly
    /// prepared and haven't been closed.
    FailedProcess((u64, CommitStrategy, Option<CommitStatusSignatures>)),
    /// The commit was properly processed but the requested finalize transaction failed.
    FailedFinalize((u64, CommitStrategy, CommitStatusSignatures)),
    /// The commit was properly processed and finalized but the requested undelegate transaction failed.
    FailedUndelegate((u64, CommitStrategy, CommitStatusSignatures)),
    /// The commit was successfully processed and finalized.
    Succeeded((u64, CommitStrategy, CommitStatusSignatures)),
}

impl fmt::Display for CommitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitStatus::Pending => write!(f, "Pending"),
            CommitStatus::Failed(bundle_id) => {
                write!(f, "Failed({})", bundle_id)
            }
            CommitStatus::BufferAndChunkPartiallyInitialized(bundle_id) => {
                write!(f, "BufferAndChunkPartiallyInitialized({})", bundle_id)
            }
            CommitStatus::BufferAndChunkInitialized(bundle_id) => {
                write!(f, "BufferAndChunkInitialized({})", bundle_id)
            }
            CommitStatus::BufferAndChunkFullyInitialized(bundle_id) => {
                write!(f, "BufferAndChunkFullyInitialized({})", bundle_id)
            }
            CommitStatus::PartOfTooLargeBundleToProcess(bundle_id) => {
                write!(f, "PartOfTooLargeBundleToProcess({})", bundle_id)
            }
            CommitStatus::FailedProcess((bundle_id, strategy, sigs)) => {
                write!(
                    f,
                    "FailedProcess({}, {}, {:?})",
                    bundle_id,
                    strategy.as_str(),
                    sigs
                )
            }
            CommitStatus::FailedFinalize((bundle_id, strategy, sigs)) => {
                write!(
                    f,
                    "FailedFinalize({}, {}, {:?})",
                    bundle_id,
                    strategy.as_str(),
                    sigs
                )
            }
            CommitStatus::FailedUndelegate((bundle_id, strategy, sigs)) => {
                write!(
                    f,
                    "FailedUndelegate({}, {}, {:?})",
                    bundle_id,
                    strategy.as_str(),
                    sigs
                )
            }
            CommitStatus::Succeeded((bundle_id, strategy, sigs)) => {
                write!(
                    f,
                    "Succeeded({}, {}, {:?})",
                    bundle_id,
                    strategy.as_str(),
                    sigs
                )
            }
        }
    }
}

impl
    TryFrom<(
        &str,
        Option<u64>,
        CommitStrategy,
        Option<CommitStatusSignatures>,
    )> for CommitStatus
{
    type Error = CommitPersistError;

    fn try_from(
        (status, bundle_id, strategy, sigs): (
            &str,
            Option<u64>,
            CommitStrategy,
            Option<CommitStatusSignatures>,
        ),
    ) -> Result<Self, Self::Error> {
        macro_rules! get_bundle_id {
            () => {
                if let Some(bundle_id) = bundle_id {
                    bundle_id
                } else {
                    return Err(CommitPersistError::CommitStatusNeedsBundleId(
                        status.to_string(),
                    ));
                }
            };
        }
        macro_rules! get_sigs {
            () => {
                if let Some(sigs) = sigs {
                    sigs
                } else {
                    return Err(CommitPersistError::CommitStatusNeedsBundleId(
                        status.to_string(),
                    ));
                }
            };
        }

        use CommitStatus::*;
        match status {
            "Pending" => Ok(Pending),
            "Failed" => Ok(Failed(get_bundle_id!())),
            "BufferAndChunkPartiallyInitialized" => {
                Ok(BufferAndChunkPartiallyInitialized(get_bundle_id!()))
            }
            "BufferAndChunkInitialized" => {
                Ok(BufferAndChunkInitialized(get_bundle_id!()))
            }
            "BufferAndChunkFullyInitialized" => {
                Ok(BufferAndChunkFullyInitialized(get_bundle_id!()))
            }
            "PartOfTooLargeBundleToProcess" => {
                Ok(PartOfTooLargeBundleToProcess(get_bundle_id!()))
            }
            "FailedProcess" => {
                Ok(FailedProcess((get_bundle_id!(), strategy, sigs)))
            }
            "FailedFinalize" => {
                Ok(FailedFinalize((get_bundle_id!(), strategy, get_sigs!())))
            }
            "FailedUndelegate" => {
                Ok(FailedUndelegate((get_bundle_id!(), strategy, get_sigs!())))
            }
            "Succeeded" => {
                Ok(Succeeded((get_bundle_id!(), strategy, get_sigs!())))
            }
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
    /// If the account was not finalized or it failed the this is `None`.
    /// If the finalize instruction was part of the process transaction then
    /// this signature is the same as [Self::process_signature].
    pub finalize_signature: Option<Signature>,
    /// The signature of the transaction undelegating the committed accounts
    /// if so requested.
    /// If the account was not undelegated or it failed the this is `None`.
    /// NOTE: this can be removed if we decide to perform the undelegation
    ///       step as part of the finalize instruction in the delegation program
    pub undelegate_signature: Option<Signature>,
}

impl CommitStatus {
    pub fn as_str(&self) -> &str {
        use CommitStatus::*;
        match self {
            Pending => "Pending",
            Failed(_) => "Failed",
            BufferAndChunkPartiallyInitialized(_) => {
                "BufferAndChunkPartiallyInitialized"
            }
            BufferAndChunkInitialized(_) => "BufferAndChunkInitialized",
            BufferAndChunkFullyInitialized(_) => {
                "BufferAndChunkFullyInitialized"
            }
            PartOfTooLargeBundleToProcess(_) => "PartOfTooLargeBundleToProcess",
            FailedProcess(_) => "FailedProcess",
            FailedFinalize(_) => "FailedFinalize",
            FailedUndelegate(_) => "FailedUndelegate",
            Succeeded(_) => "Succeeded",
        }
    }

    pub fn bundle_id(&self) -> Option<u64> {
        use CommitStatus::*;
        match self {
            Failed(bundle_id)
            | BufferAndChunkPartiallyInitialized(bundle_id)
            | BufferAndChunkInitialized(bundle_id)
            | BufferAndChunkFullyInitialized(bundle_id)
            | PartOfTooLargeBundleToProcess(bundle_id)
            | FailedProcess((bundle_id, _, _))
            | FailedFinalize((bundle_id, _, _))
            | FailedUndelegate((bundle_id, _, _))
            | Succeeded((bundle_id, _, _)) => Some(*bundle_id),
            Pending => None,
        }
    }

    pub fn signatures(&self) -> Option<CommitStatusSignatures> {
        use CommitStatus::*;
        match self {
            FailedProcess((_, _, sigs)) => sigs.as_ref().cloned(),
            FailedFinalize((_, _, sigs)) => Some(sigs.clone()),
            Succeeded((_, _, sigs)) => Some(sigs.clone()),
            _ => None,
        }
    }

    pub fn commit_strategy(&self) -> CommitStrategy {
        use CommitStatus::*;
        match self {
            Pending => CommitStrategy::Undetermined,
            Failed(_) => CommitStrategy::Undetermined,
            BufferAndChunkPartiallyInitialized(_)
            | BufferAndChunkInitialized(_)
            | BufferAndChunkFullyInitialized(_) => CommitStrategy::FromBuffer,
            PartOfTooLargeBundleToProcess(_) => CommitStrategy::Undetermined,
            FailedProcess((_, strategy, _)) => *strategy,
            FailedFinalize((_, strategy, _)) => *strategy,
            FailedUndelegate((_, strategy, _)) => *strategy,
            Succeeded((_, strategy, _)) => *strategy,
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
