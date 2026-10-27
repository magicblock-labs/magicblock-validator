use program::{
    outbox::{ExecutionStage, PendingTransaction, TwoStageProgress},
    outbox_intent_bundles::OutboxIntentBundleStatus,
};
use solana_signature::Signature;

/// The stored outbox encoding still names single/two-stage execution. Keep
/// those names at this boundary so existing accepted intents remain readable.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingExecution {
    pub transaction: PendingTransaction,
    pub confirmed_commit: Option<Signature>,
    pub has_follow_up: bool,
}

impl PendingExecution {
    pub(crate) fn from_status(status: &OutboxIntentBundleStatus) -> Option<Self> {
        let OutboxIntentBundleStatus::Executing(stage) = status else { return None };
        Some(match stage {
            ExecutionStage::SingleStage(transaction) => Self {
                transaction: *transaction,
                confirmed_commit: None,
                has_follow_up: false,
            },
            ExecutionStage::TwoStage(TwoStageProgress::Committing(transaction)) => Self {
                transaction: *transaction,
                confirmed_commit: None,
                has_follow_up: true,
            },
            ExecutionStage::TwoStage(TwoStageProgress::Finalizing { commit, finalize }) => Self {
                transaction: *finalize,
                confirmed_commit: Some(*commit),
                has_follow_up: false,
            },
        })
    }

    pub(crate) fn into_stage(self) -> ExecutionStage {
        if let Some(commit) = self.confirmed_commit {
            ExecutionStage::TwoStage(TwoStageProgress::Finalizing {
                commit,
                finalize: self.transaction,
            })
        } else if self.has_follow_up {
            ExecutionStage::TwoStage(TwoStageProgress::Committing(self.transaction))
        } else {
            ExecutionStage::SingleStage(self.transaction)
        }
    }
}
