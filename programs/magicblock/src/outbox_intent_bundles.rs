use solana_signature::Signature;

use crate::magic_scheduled_base_intent::ScheduledIntentBundle;

// TODO(edwin): naming Outbox/Pending
pub struct OutboxIntentBundle {
    // TODO(edwin): define visability
    pub intent_bundle: ScheduledIntentBundle,
    // TODO(edwin): define visability
    pub status: OutboxIntentBundleStatus,
}

pub enum OutboxIntentBundleStatus {
    Accepted,
    Executing(ExecutionStage),
}

pub enum ExecutionStage {
    SingleStage(Signature),
    TwoStage(TwoStageProgress),
}

pub enum TwoStageProgress {
    Committing(Signature),
    Finalizing {
        commit: Signature,
        finalize: Signature,
    },
}
