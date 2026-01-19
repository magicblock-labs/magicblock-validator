use std::ops::Deref;

use magicblock_metrics::metrics;
use magicblock_program::magic_scheduled_base_intent::{
    MagicBaseIntent, ScheduledIntentBundle,
};

// TODO: should be removed once cranks are supported
// Ideally even now OffChain/"Manual" commits should be triggered via Tx
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerType {
    OnChain,
    OffChain,
}

// TODO(edwin): can be removed?
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleIntentBundleWrapper {
    pub inner: ScheduledIntentBundle,
    pub trigger_type: TriggerType,
}

impl metrics::LabelValue for ScheduleIntentBundleWrapper {
    fn value(&self) -> &str {
        match &self.inner.intent_bundle {
            MagicBaseIntent::BaseActions(_) => "actions",
            MagicBaseIntent::Commit(_) => "commit",
            MagicBaseIntent::CommitAndUndelegate(_) => "commit_and_undelegate",
        }
    }
}

impl Deref for ScheduleIntentBundleWrapper {
    type Target = ScheduledIntentBundle;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
