use serde::{Deserialize, Serialize};
use solana_hash::Hash;
use solana_signature::Signature;

/// A transaction that was sent but not yet confirmed, along with the
/// blockhash it was built with. The blockhash is needed on recovery to
/// tell apart "may still land" (blockhash still valid) from "guaranteed
/// dead" (blockhash expired) when the signature isn't found on-chain.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct PendingTransaction {
    pub signature: Signature,
    pub blockhash: Hash,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum ExecutionStage {
    SingleStage(PendingTransaction),
    TwoStage(TwoStageProgress),
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
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
    ) -> Result<(), &'static str> {
        match (self, stage) {
            // Current sig wasn't confirmed, we replace it with new attempt
            (Self::SingleStage(ref mut this_sig), Self::SingleStage(sig)) => {
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
                return Err("cannot transition from SingleStage to Finalizing");
            }
            // Transitions within TwoStage states
            (Self::TwoStage(ref mut this), Self::TwoStage(value)) => {
                this.apply_stage_transition(value)?;
            }
            // TwoStage can't be downgraded into SingleStage
            (Self::TwoStage(_), Self::SingleStage(_)) => {
                return Err(
                    "cannot change execution type from TwoStage to SingleStage",
                );
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
    ) -> Result<(), &'static str> {
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
                        "commit signature mismatch on advance to Finalizing",
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
                        "commit signature can't be replaced in Finalize stage",
                    );
                }

                Self::Finalizing { commit, finalize }
            }
            // Incorrect state transition
            (Self::Finalizing { .. }, Self::Committing(_)) => {
                return Err(
                    "downgrade from Finalizing to Committing not permitted",
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
