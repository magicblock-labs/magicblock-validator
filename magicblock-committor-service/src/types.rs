use std::ops::Deref;

use magicblock_metrics::metrics;
use magicblock_program::magic_scheduled_base_intent::{
    MagicBaseIntent, ScheduledBaseIntent,
};

// TODO: should be removed once cranks are supported
// Ideally even now OffChain/"Manual" commits should be triggered via Tx
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerType {
    OnChain,
    OffChain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledBaseIntentWrapper {
    pub inner: ScheduledBaseIntent,
    pub trigger_type: TriggerType,
}

impl metrics::LabelValue for ScheduledBaseIntentWrapper {
    fn value(&self) -> &str {
        match &self.inner.base_intent {
            MagicBaseIntent::BaseActions(_) => "actions",
            MagicBaseIntent::Commit(_) => "commit",
            MagicBaseIntent::CommitAndUndelegate(_) => "commit_and_undelegate",
        }
    }
}

impl Deref for ScheduledBaseIntentWrapper {
    type Target = ScheduledBaseIntent;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
