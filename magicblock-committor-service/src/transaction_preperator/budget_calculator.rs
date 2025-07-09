use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, instruction::Instruction,
};

use crate::{compute_budget::Budget, ComputeBudgetConfig};

// TODO(edwin): rename
pub struct ComputeBudgetV1 {
    /// Total compute budget
    pub compute_budget: u32,
    pub compute_unit_price: u64,
}

impl ComputeBudgetV1 {
    /// Needed just to create dummy ixs, and evaluate size
    fn dummy() -> Self {
        Self {
            compute_budget: 0,
            compute_unit_price: 0,
        }
    }
}

pub trait ComputeBudgetCalculator {
    fn budget_instructions(budget: ComputeBudgetV1) -> [Instruction; 2];

    /// Calculate budget for commit transaction
    fn calculate_commit_budget(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> ComputeBudgetV1;
    /// Calculate budget for finalze transaction
    fn calculate_finalize_budget(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> ComputeBudgetV1;
}

/// V1 implementation, works with TransactionPreparator V1
/// Calculations for finalize may include cases for
pub struct ComputeBudgetCalculatorV1 {
    compute_budget_config: ComputeBudgetConfig,
}

impl ComputeBudgetCalculatorV1 {
    pub fn new(config: ComputeBudgetConfig) -> Self {
        Self {
            compute_budget_config: config,
        }
    }
}

impl ComputeBudgetCalculator for ComputeBudgetCalculatorV1 {
    /// Calculate compute budget for V1 commit transaction
    /// This includes only compute for account commits
    fn calculate_commit_budget(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> ComputeBudgetV1 {
        todo!()
    }

    fn calculate_finalize_budget(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> ComputeBudgetV1 {
        todo!()
    }

    fn budget_instructions(budget: ComputeBudgetV1) -> [Instruction; 2] {
        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(
                budget.compute_budget,
            );
        let compute_unit_price_ix =
            ComputeBudgetInstruction::set_compute_unit_price(
                budget.compute_unit_price,
            );

        [compute_budget_ix, compute_unit_price_ix]
    }
}

//  We need to create an optimal TX
// Optimal tx - [ComputeBudget, Args(acc1), Buffer(acc2), Args(Action))]
// Estimate actual budget
// Recreate TX
//
