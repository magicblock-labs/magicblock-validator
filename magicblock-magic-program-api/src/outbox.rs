use serde::{Deserialize, Serialize};
use solana_signature::Signature;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum ExecutionStage {
    SingleStage(Signature),
    TwoStage(TwoStageProgress),
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum TwoStageProgress {
    Committing(Signature),
    Finalizing {
        commit: Signature,
        finalize: Signature,
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
                Self::Committing(this_sig),
                Self::Finalizing { commit, finalize },
            ) => {
                if this_sig != &commit {
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
}
