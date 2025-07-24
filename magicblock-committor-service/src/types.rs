use std::fmt;

use magicblock_program::{
    magic_scheduled_l1_message::ScheduledL1Message, FeePayerAccount,
};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::Instruction;

use crate::CommitInfo;

/// The kind of instructions included for the particular [CommitInfo]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstructionsKind {
    /// The commit is processed only and may include the finalize instruction
    Process,
    /// The buffers to facilitate are closed, but processing occurred as part
    /// of another set of instructions
    CloseBuffers,
    /// The commit is processed and the buffers closed all as part of this set
    /// of instructions
    ProcessAndCloseBuffers,
    /// The commit is processed previously and only finalized by this set of
    /// instructions
    Finalize,
    /// The commit is processed and finalized previously and the committee is
    /// undelegated by this set of instructions
    Undelegate,
}

impl InstructionsKind {
    pub fn is_processing(&self) -> bool {
        matches!(
            self,
            InstructionsKind::Process
                | InstructionsKind::ProcessAndCloseBuffers
        )
    }
}

#[derive(Debug)]
pub struct InstructionsForCommitable {
    pub instructions: Vec<Instruction>,
    pub commit_info: CommitInfo,
    pub kind: InstructionsKind,
}

impl fmt::Display for InstructionsForCommitable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "InstructionsForCommitable {{
    instructions.len: {},
    commit_info: {}
    kind: {:?}
}}",
            self.instructions.len(),
            self.commit_info.pubkey(),
            self.kind
        )
    }
}

// TODO: should be removed once cranks are supported
// Ideally even now OffChain/"Manual" commits should be triggered via Tx
#[derive(Clone, Copy, Debug)]
pub enum TriggerType {
    OnChain,
    OffChain,
}

#[derive(Clone, Debug)]
pub struct ScheduledL1MessageWrapper {
    pub scheduled_l1_message: ScheduledL1Message,
    pub feepayers: Vec<FeePayerAccount>,
    pub excluded_pubkeys: Vec<Pubkey>,
    pub trigger_type: TriggerType,
}
