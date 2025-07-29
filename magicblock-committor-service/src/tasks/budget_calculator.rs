use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, instruction::Instruction,
};

use crate::ComputeBudgetConfig;

// TODO(edwin): rename
pub struct ComputeBudgetV1 {
    /// Total compute budget
    pub compute_budget: u32,
    pub compute_unit_price: u64,
}
