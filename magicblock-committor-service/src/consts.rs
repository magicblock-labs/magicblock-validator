// https://solana.com/docs/core/transactions#transaction-size

use magicblock_committor_program::{
    consts::MAX_INSTRUCTION_DATA_SIZE,
    instruction::IX_WRITE_SIZE_WITHOUT_CHUNKS,
};

const BUDGET_SET_COMPUTE_UNIT_PRICE_BYTES: u16 = (1 + 8) * 8;
const BUDGET_SET_COMPUTE_UNIT_LIMIT_BYTES: u16 = (1 + 4) * 8;

/// The maximum size of a chunk that can be written as part of a single transaction
pub(super) const MAX_WRITE_CHUNK_SIZE: u16 = MAX_INSTRUCTION_DATA_SIZE
    - IX_WRITE_SIZE_WITHOUT_CHUNKS
    - BUDGET_SET_COMPUTE_UNIT_PRICE_BYTES
    - BUDGET_SET_COMPUTE_UNIT_LIMIT_BYTES;
