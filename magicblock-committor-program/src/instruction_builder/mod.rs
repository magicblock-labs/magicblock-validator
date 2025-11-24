use borsh::BorshSerialize;
use solana_program::instruction::{AccountMeta, Instruction};

use crate::instruction::CommittorInstruction;

pub mod close_buffer;
pub mod init_buffer;
pub mod realloc_buffer;
pub mod write_buffer;

fn build_instruction(
    ix: CommittorInstruction,
    accounts: Vec<AccountMeta>,
) -> Instruction {
    Instruction::new_with_bytes(
        crate::id(),
        &ix.try_to_vec()
            .expect("Serialization of instruction should never fail"),
        accounts,
    )
}
