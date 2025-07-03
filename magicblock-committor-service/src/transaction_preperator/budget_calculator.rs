use solana_sdk::instruction::Instruction;
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use crate::compute_budget::Budget;
use crate::ComputeBudgetConfig;

// TODO(edwin): rename
struct ComputeBudgetV1 {
    /// Total compute budget
    pub compute_budget: u32,
    pub compute_unit_price: u64
}

pub trait ComputeBudgetCalculator {
    fn instruction(budget: ComputeBudgetV1) -> Instruction;

    /// Calculate budget for commit transaction
    fn calculate_commit_budget(&self, l1_message: &ScheduledL1Message) -> ComputeBudgetV1;
    /// Calculate budget for finalze transaction
    fn calculate_finalize_budget(&self, l1_message: &ScheduledL1Message) -> ComputeBudgetV1;
}

/// V1 implementation, works with TransactionPreparator V1
/// Calculations for finalize may include cases for
pub struct ComputeBudgetCalculatorV1 {
    compute_budget_config: ComputeBudgetConfig
}

impl ComputeBudgetCalculatorV1 {
    pub fn new(config: ComputeBudgetConfig) -> Self {
        Self {
            compute_budget_config: config
        }
    }
}

impl ComputeBudgetCalculator for ComputeBudgetCalculatorV1 {
    /// Calculate compute budget for V1 commit transaction
    /// This includes only compute for account commits
    fn calculate_commit_budget(&self, l1_message: &ScheduledL1Message) -> ComputeBudgetV1 {
        todo!()
    }

    fn calculate_finalize_budget(&self, l1_message: &ScheduledL1Message) -> ComputeBudgetV1 {
        todo!()
    }

    fn instruction(budget: ComputeBudgetV1) -> Instruction {
        
    }
}

///