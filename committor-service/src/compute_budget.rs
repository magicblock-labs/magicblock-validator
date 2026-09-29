use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::Instruction;

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
        .saturating_add(self.base_budget)
    }

    pub fn instructions(&self, bytes_count: usize) -> Vec<Instruction> {
        instructions(self.total_budget(bytes_count), self.compute_unit_price)
    }
}

#[derive(Debug, Clone)]
pub struct ComputeBudgetConfig {
    pub compute_unit_price: u64,
    pub buffer_init: BufferWithReallocBudget,
    pub buffer_realloc: BufferWithReallocBudget,
    pub buffer_write: BufferWriteChunkBudget,
}

impl ComputeBudgetConfig {
    pub fn new(compute_unit_price: u64) -> Self {
        Self {
            compute_unit_price,
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
