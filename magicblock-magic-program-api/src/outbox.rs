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
            (Self::SingleStage(ref mut this_sig), Self::SingleStage(sig)) => {
                *this_sig = sig;
            }
            // TODO(edwin): validate this case,
            (
                this @ Self::SingleStage(_),
                val @ Self::TwoStage(TwoStageProgress::Committing(_)),
            ) => {
                *this = val;
            }
            (
                Self::SingleStage(_),
                Self::TwoStage(TwoStageProgress::Finalizing { .. }),
            ) => {
                return Err("cannot transition from SingleStage to Finalizing");
            }
            (Self::TwoStage(ref mut this), Self::TwoStage(value)) => {
                this.apply_stage_transition(value)?;
            }
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
        match (self, stage) {
            (Self::Committing(this_sig), Self::Committing(value)) => {
                *this_sig = value;
            }
            (
                this @ Self::Committing(this_sig),
                Self::Finalizing { commit, finalize },
            ) => {
                if this_sig != &commit {
                    return Err(
                        "commit signature mismatch on advance to Finalizing",
                    );
                }
                *this = Self::Finalizing { commit, finalize };
            }
            (
                Self::Finalizing { commit: this_commit, .. },
                Self::Finalizing {
                    commit, ..
                },
            ) => {
                *this_commit = commit;
            }
            (Self::Finalizing { .. }, Self::Committing(_)) => {
                return Err(
                    "downgrade from Finalizing to Committing not permitted",
                );
            }
        }
        Ok(())
    }
}
