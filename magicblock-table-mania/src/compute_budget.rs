use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, instruction::Instruction,
};

pub const CREATE_AND_EXTEND_TABLE_CUS: u32 = 2_100;
pub const EXTEND_TABLE_CUS: u32 = 900;

#[derive(Clone)]
pub struct TableManiaComputeBudget {
    compute_budget: u32,
    compute_unit_price: u64,
}

impl TableManiaComputeBudget {
    fn init() -> Self {
        Self {
            compute_budget: CREATE_AND_EXTEND_TABLE_CUS,
            compute_unit_price: 1_000_000,
        }
    }

    fn extend() -> Self {
        Self {
            compute_budget: EXTEND_TABLE_CUS,
            compute_unit_price: 1_000_000,
        }
    }

    pub fn instructions(&self) -> (Instruction, Instruction) {
        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(
                self.compute_budget,
            );
        let compute_unit_price_ix =
            ComputeBudgetInstruction::set_compute_unit_price(
                self.compute_unit_price,
            );
        (compute_budget_ix, compute_unit_price_ix)
    }
}

#[derive(Clone)]
pub struct TableManiaComputeBudgets {
    pub init: TableManiaComputeBudget,
    pub extend: TableManiaComputeBudget,
}

impl Default for TableManiaComputeBudgets {
    fn default() -> Self {
        Self {
            init: TableManiaComputeBudget::init(),
            extend: TableManiaComputeBudget::extend(),
        }
    }
}
