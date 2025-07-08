use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, instruction::Instruction,
};

/// Compute units required to create and extend a lookup table, with the initial
/// pubkeys. This is the same no matter how many pubkeys are added to the table
/// initially.
pub const CREATE_AND_EXTEND_TABLE_CUS: u32 = 2_100;
/// Compute units required to extend a lookup table with additional pubkeys
/// This is the same no matter how many pubkeys are added to the table.
pub const EXTEND_TABLE_CUS: u32 = 900;
/// Compute units required to deactivate a lookup table.
pub const DEACTIVATE_TABLE_CUS: u32 = 750;
/// Compute units required to close a lookup table.
pub const CLOSE_TABLE_CUS: u32 = 750;

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

    fn deactivate() -> Self {
        Self {
            compute_budget: DEACTIVATE_TABLE_CUS,
            compute_unit_price: 1_000_000,
        }
    }

    fn close() -> Self {
        Self {
            compute_budget: CLOSE_TABLE_CUS,
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

/// Provides the compute budgets and prices for lookup table instructions.
/// The CUs were obtained by running the instruction against the solana test validator.
/// The price is merely a guess at an amount that is not excessive yet will let all
/// transactions succeed.
#[derive(Clone)]
pub struct TableManiaComputeBudgets {
    pub init: TableManiaComputeBudget,
    pub extend: TableManiaComputeBudget,
    pub deactivate: TableManiaComputeBudget,
    pub close: TableManiaComputeBudget,
}

impl Default for TableManiaComputeBudgets {
    fn default() -> Self {
        Self {
            init: TableManiaComputeBudget::init(),
            extend: TableManiaComputeBudget::extend(),
            deactivate: TableManiaComputeBudget::deactivate(),
            close: TableManiaComputeBudget::close(),
        }
    }
}
