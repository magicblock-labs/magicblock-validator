use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message, FeePayerAccount,
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
pub struct ScheduledL1MessageWrapper {
    pub scheduled_l1_message: ScheduledL1Message,
    pub feepayers: Vec<FeePayerAccount>,
    pub excluded_pubkeys: Vec<Pubkey>,
    pub trigger_type: TriggerType,
}
