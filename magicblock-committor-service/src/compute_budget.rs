use solana_sdk::{
    compute_budget::ComputeBudgetInstruction, instruction::Instruction,
};

// -----------------
// Budgets
// -----------------
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    base_budget: u32,
    per_committee: u32,
    compute_unit_price: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            base_budget: 80_000,
            per_committee: 45_000,
            compute_unit_price: 1_000_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BufferWithReallocBudget {
    base_budget: u32,
    per_realloc_ix: u32,
    compute_unit_price: u64,
}

impl BufferWithReallocBudget {
    fn total_budget(&self, realloc_ixs_count: u32) -> u32 {
        self.base_budget + (self.per_realloc_ix * realloc_ixs_count)
    }

    pub fn instructions(&self, realloc_ixs_count: usize) -> Vec<Instruction> {
        let realloc_ixs_count =
            u32::try_from(realloc_ixs_count).unwrap_or(u32::MAX);

        instructions(
            self.total_budget(realloc_ixs_count),
            self.compute_unit_price,
        )
    }
}

#[derive(Debug, Clone)]
pub struct BufferWriteChunkBudget {
    base_budget: u32,
    per_byte: usize,
    compute_unit_price: u64,
}

impl BufferWriteChunkBudget {
    fn total_budget(&self, bytes_count: usize) -> u32 {
        u32::try_from(
            self.per_byte
                .checked_mul(bytes_count)
                .unwrap_or(u32::MAX as usize),
        )
        .unwrap_or(u32::MAX)
        .checked_add(self.base_budget)
        .unwrap_or(u32::MAX)
    }

    pub fn instructions(&self, bytes_count: usize) -> Vec<Instruction> {
        instructions(self.total_budget(bytes_count), self.compute_unit_price)
    }
}

// -----------------
// ComputeBudgetConfig
// -----------------
#[derive(Debug, Clone)]
pub struct ComputeBudgetConfig {
    pub compute_unit_price: u64,
    pub args_process: Budget,
    pub finalize: Budget,
    pub buffer_close: Budget,
    /// The budget used for processing and process + closing a buffer.
    /// Since we mix pure process and process + close instructions, we need to
    /// assume the worst case and use the process + close budget for all.
    pub buffer_process_and_close: Budget,
    pub undelegate: Budget,
    pub buffer_init: BufferWithReallocBudget,
    pub buffer_realloc: BufferWithReallocBudget,
    pub buffer_write: BufferWriteChunkBudget,
}

impl ComputeBudgetConfig {
    pub fn new(compute_unit_price: u64) -> Self {
        Self {
            compute_unit_price,
            args_process: Budget {
                compute_unit_price,
                base_budget: 80_000,
                per_committee: 35_000,
            },
            buffer_close: Budget {
                compute_unit_price,
                base_budget: 10_000,
                per_committee: 25_000,
            },
            buffer_process_and_close: Budget {
                compute_unit_price,
                base_budget: 40_000,
                per_committee: 45_000,
            },
            finalize: Budget {
                compute_unit_price,
                base_budget: 80_000,
                per_committee: 25_000,
            },
            undelegate: Budget {
                compute_unit_price,
                base_budget: 40_000,
                per_committee: 35_000,
            },
            buffer_init: BufferWithReallocBudget {
                base_budget: 12_000,
                per_realloc_ix: 6_000,
                compute_unit_price: 1_000_000,
            },
            buffer_realloc: BufferWithReallocBudget {
                base_budget: 12_000,
                per_realloc_ix: 6_000,
                compute_unit_price: 1_000_000,
            },
            buffer_write: BufferWriteChunkBudget {
                base_budget: 10_000,
                per_byte: 3,
                compute_unit_price: 1_000_000,
            },
        }
    }
}

impl ComputeBudgetConfig {
    pub fn args_process_budget(&self) -> ComputeBudget {
        ComputeBudget::Process(self.args_process)
    }
    pub fn buffer_close_budget(&self) -> ComputeBudget {
        ComputeBudget::Close(self.buffer_close)
    }
    pub fn buffer_process_and_close_budget(&self) -> ComputeBudget {
        ComputeBudget::ProcessAndClose(self.buffer_process_and_close)
    }
    pub fn finalize_budget(&self) -> ComputeBudget {
        ComputeBudget::Finalize(self.finalize)
    }
    pub fn undelegate_budget(&self) -> ComputeBudget {
        ComputeBudget::Undelegate(self.undelegate)
    }
}

// -----------------
// ComputeBudget
// -----------------
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComputeBudget {
    Process(Budget),
    Close(Budget),
    ProcessAndClose(Budget),
    Finalize(Budget),
    Undelegate(Budget),
}

impl ComputeBudget {
    fn base_budget(&self) -> u32 {
        use ComputeBudget::*;
        match self {
            Process(budget) => budget.base_budget,
            Close(budget) => budget.base_budget,
            ProcessAndClose(budget) => budget.base_budget,
            Finalize(budget) => budget.base_budget,
            Undelegate(budget) => budget.base_budget,
        }
    }

    fn per_committee(&self) -> u32 {
        use ComputeBudget::*;
        match self {
            Process(budget) => budget.per_committee,
            Close(budget) => budget.per_committee,
            ProcessAndClose(budget) => budget.per_committee,
            Finalize(budget) => budget.per_committee,
            Undelegate(budget) => budget.per_committee,
        }
    }

    pub fn compute_unit_price(&self) -> u64 {
        use ComputeBudget::*;
        match self {
            Process(budget) => budget.compute_unit_price,
            Close(budget) => budget.compute_unit_price,
            ProcessAndClose(budget) => budget.compute_unit_price,
            Finalize(budget) => budget.compute_unit_price,
            Undelegate(budget) => budget.compute_unit_price,
        }
    }

    fn total_budget(&self, committee_count: u32) -> u32 {
        self.per_committee()
            .checked_mul(committee_count)
            .and_then(|product| product.checked_add(self.base_budget()))
            .unwrap_or(u32::MAX)
    }

    pub fn instructions(&self, committee_count: usize) -> Vec<Instruction> {
        let committee_count =
            u32::try_from(committee_count).unwrap_or(u32::MAX);

        instructions(
            self.total_budget(committee_count),
            self.compute_unit_price(),
        )
    }
}

fn instructions(
    compute_budget: u32,
    compute_unit_price: u64,
) -> Vec<Instruction> {
    let compute_budget_ix =
        ComputeBudgetInstruction::set_compute_unit_limit(compute_budget);
    let compute_unit_price_ix =
        ComputeBudgetInstruction::set_compute_unit_price(compute_unit_price);
    vec![compute_budget_ix, compute_unit_price_ix]
}
