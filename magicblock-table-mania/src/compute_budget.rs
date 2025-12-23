use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::Instruction;

/// We multiply the CU values that we determined since we've seen compute budget exceeded errors
/// if we use them as is. The only way to determine CUs for a particular transaction 100% is
/// to simulate that exact transaction.
/// This would make table management even slower, but is an option to consider for the future.
/// The multiplier is based on the observation that [CREATE_AND_EXTEND_TABLE_CUS] were _safe_ at
/// 16K-17K CUs which we slightly exceed if we multiply by this multiplier.
const SAFETY_MULTIPLIER: u32 = 12;

/// Compute units required to create and extend a lookup table, with the initial
/// pubkeys. This is the same no matter how many pubkeys are added to the table
/// initially.
/// The added 400 ensure we don't run out of CUs due to variation in the PDA derivation.
/// Only the create table instruction has one, see
/// https://github.com/solana-program/address-lookup-table/blob/main/program/src/processor.rs .
/// This test repo (https://github.com/thlorenz/create-program-address-cus) verified
/// that the variation is around 25 CUs, but to be on the safe side we add 10x that.
pub const CREATE_AND_EXTEND_TABLE_CUS: u32 = (2_400 + 400) * SAFETY_MULTIPLIER;
/// Compute units required to extend a lookup table with additional pubkeys
/// This is the same no matter how many pubkeys are added to the table.
pub const EXTEND_TABLE_CUS: u32 = 1_300 * SAFETY_MULTIPLIER;
/// Compute units required to deactivate a lookup table.
pub const DEACTIVATE_TABLE_CUS: u32 = 1_050 * SAFETY_MULTIPLIER;
/// Compute units required to close a lookup table.
pub const CLOSE_TABLE_CUS: u32 = 1_050 * SAFETY_MULTIPLIER;

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
