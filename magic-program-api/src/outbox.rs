use core::fmt;

use serde::{Deserialize, Serialize};
use solana_hash::Hash;
use solana_signature::Signature;
use wincode::{SchemaRead, SchemaWrite};

/// A transaction that was sent but not yet confirmed, along with the
/// blockhash it was built with. The blockhash is needed on recovery to
/// tell apart "may still land" (blockhash still valid) from "guaranteed
/// dead" (blockhash expired) when the signature isn't found on-chain.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "backward-compat"), derive(SchemaRead, SchemaWrite))]
pub struct PendingTransaction {
    pub signature: Signature,
    pub blockhash: Hash,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "backward-compat"), derive(SchemaRead, SchemaWrite))]
pub enum ExecutionStage {
    SingleStage(PendingTransaction),
    TwoStage(TwoStageProgress),
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "backward-compat"), derive(SchemaRead, SchemaWrite))]
pub enum TwoStageProgress {
    Committing(PendingTransaction),
    Finalizing {
        commit: Signature,
        finalize: PendingTransaction,
    },
}

impl ExecutionStage {
    pub fn apply_stage_transition(
        &mut self,
        stage: ExecutionStage,
    ) -> Result<(), StageTransitionError> {
        match (self, stage) {
            // Current sig wasn't confirmed, we replace it with new attempt
            (Self::SingleStage(this_sig), Self::SingleStage(sig)) => {
                *this_sig = sig;
            }
            // TODO(edwin): validate this case,
            // We tried SingleStage execution, but failed (CpiLimit, etc)
            // We patch it moving to TwoStage flow
            (
                this @ Self::SingleStage(_),
                val @ Self::TwoStage(TwoStageProgress::Committing(_)),
            ) => {
                *this = val;
            }
            // Only transition to TwoStageProgress::Committing is valid from SingleStage
            (
                Self::SingleStage(_),
                Self::TwoStage(TwoStageProgress::Finalizing { .. }),
            ) => {
                return Err(StageTransitionError::SingleStageToFinalizingError);
            }
            // Transitions within TwoStage states
            (Self::TwoStage(this), Self::TwoStage(value)) => {
                this.apply_stage_transition(value)?;
            }
            // TwoStage can't be downgraded into SingleStage
            (Self::TwoStage(_), Self::SingleStage(_)) => {
                return Err(StageTransitionError::TwoStageToSingleStageError);
            }
        }

        Ok(())
    }

    pub fn pending_transaction(&self) -> &PendingTransaction {
        match self {
            Self::SingleStage(pending) => pending,
            Self::TwoStage(value) => value.pending_transaction(),
        }
    }
}

impl TwoStageProgress {
    fn apply_stage_transition(
        &mut self,
        stage: TwoStageProgress,
    ) -> Result<(), StageTransitionError> {
        let new_state = match (&self, stage) {
            // Current sig didn't succeed on Base, we replace it with new attempt
            (Self::Committing(_), Self::Committing(new_sig)) => {
                Self::Committing(new_sig)
            }
            // Commit was successfully executed and now we move on to Finalizing
            (
                Self::Committing(this_pending),
                Self::Finalizing { commit, finalize },
            ) => {
                if this_pending.signature != commit {
                    return Err(
                        StageTransitionError::CommitSignatureMismatchError,
                    );
                }

                Self::Finalizing { commit, finalize }
            }
            // Current finalize sig wasn't confirmed, we replace it with new attempt
            (
                Self::Finalizing {
                    commit: this_commit,
                    ..
                },
                Self::Finalizing { commit, finalize },
            ) => {
                if this_commit != &commit {
                    return Err(
                        StageTransitionError::CommitSignatureReplacementError,
                    );
                }

                Self::Finalizing { commit, finalize }
            }
            // Incorrect state transition
            (Self::Finalizing { .. }, Self::Committing(_)) => {
                return Err(
                    StageTransitionError::FinalizingToCommittingDowngradeError,
                );
            }
        };

        *self = new_state;
        Ok(())
    }

    pub fn pending_transaction(&self) -> &PendingTransaction {
        match self {
            Self::Committing(pending) => pending,
            Self::Finalizing { finalize, .. } => finalize,
        }
    }
}

/// Rejected [`ExecutionStage`]/[`TwoStageProgress`] transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageTransitionError {
    /// `SingleStage` can only advance into `TwoStage::Committing`.
    SingleStageToFinalizingError,
    /// `TwoStage` execution can't be downgraded back to `SingleStage`.
    TwoStageToSingleStageError,
    /// The commit signature recorded in `Finalizing` didn't match the one
    /// being advanced from `Committing`.
    CommitSignatureMismatchError,
    /// `Finalizing`'s commit signature is fixed once recorded.
    CommitSignatureReplacementError,
    /// `Finalizing` can't regress back to `Committing`.
    FinalizingToCommittingDowngradeError,
}

impl fmt::Display for StageTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::SingleStageToFinalizingError => {
                "cannot transition from SingleStage to Finalizing"
            }
            Self::TwoStageToSingleStageError => {
                "cannot change execution type from TwoStage to SingleStage"
            }
            Self::CommitSignatureMismatchError => {
                "commit signature mismatch on advance to Finalizing"
            }
            Self::CommitSignatureReplacementError => {
                "commit signature can't be replaced in Finalize stage"
            }
            Self::FinalizingToCommittingDowngradeError => {
                "downgrade from Finalizing to Committing not permitted"
            }
        };
        f.write_str(msg)
    }
}
