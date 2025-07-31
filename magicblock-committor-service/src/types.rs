use std::ops::Deref;

use magicblock_program::{
    magic_scheduled_base_intent::ScheduledBaseIntent, FeePayerAccount,
};
use solana_pubkey::Pubkey;

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
    pub feepayers: Vec<FeePayerAccount>,
    pub excluded_pubkeys: Vec<Pubkey>,
    pub trigger_type: TriggerType,
}

impl Deref for ScheduledBaseIntentWrapper {
    type Target = ScheduledBaseIntent;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
