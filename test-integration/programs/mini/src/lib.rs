#![allow(unexpected_cfgs)]
use std::str::FromStr;

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, pubkey::Pubkey,
};

pub mod common;
pub mod instruction;
pub mod processor;
pub mod sdk;
pub mod state;

use instruction::MiniInstruction;
use processor::Processor;

static ID: Option<&str> = option_env!("MINI_PROGRAM_ID");
pub fn id() -> Pubkey {
    Pubkey::from_str(
        ID.unwrap_or("Mini111111111111111111111111111111111111111"),
    )
    .expect("Invalid program ID")
}

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let instruction = MiniInstruction::try_from(instruction_data)?;
    Processor::process(program_id, accounts, &instruction)
}

// NOTE: The previous `solana-program-test`-based unit tests were removed during
// the Agave 4.0 upgrade. `solana-program-test` embeds the full upstream
// `solana-runtime` bank, which is incompatible with the stripped-down
// magicblock SVM fork (`../runtime`). The mini program's behavior is exercised
// by the `test-cloning` and `test-chainlink` integration suites instead.
