use std::fmt;

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
