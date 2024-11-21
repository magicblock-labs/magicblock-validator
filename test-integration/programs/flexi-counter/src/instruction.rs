use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::state::FlexiCounter;

/// The counter has both mul and add instructions in order to facilitate tests where
/// order matters. For example in the case of the following operations:
/// +4, *2
/// if the *2 operation runs before the add then we end up with 4 as a result instead of
/// the correct result 8.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum FlexiCounterInstruction {
    /// Creates a FlexiCounter account.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that is creating the account.
    /// 1. `[write]` The counter PDA account that will be created.
    /// 2. `[]` The system program account.
    Init { label: String, bump: u8 },

    /// Updates the FlexiCounter by adding the count to it.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that is creating the account.
    /// 1. `[write]` The counter PDA account that will be updated.
    Add { count: u8 },

    /// Updates the FlexiCounter by multiplying  the count with the multiplier.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that is creating the account.
    /// 1. `[write]` The counter PDA account that will be updated.
    Mul { multiplier: u8 },
}

pub fn create_init_ix(payer: Pubkey, label: String) -> Instruction {
    let program_id = &crate::id();
    let (pda, bump) = FlexiCounter::pda(&payer);
    let accounts = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(pda, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Init { label, bump },
        accounts,
    )
}

pub fn create_add_ix(payer: Pubkey, count: u8) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts =
        vec![AccountMeta::new(payer, true), AccountMeta::new(pda, false)];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Add { count },
        accounts,
    )
}

pub fn create_mul_ix(payer: Pubkey, multiplier: u8) -> Instruction {
    let program_id = &crate::id();
    let (pda, _) = FlexiCounter::pda(&payer);
    let accounts =
        vec![AccountMeta::new(payer, true), AccountMeta::new(pda, false)];
    Instruction::new_with_borsh(
        *program_id,
        &FlexiCounterInstruction::Mul { multiplier },
        accounts,
    )
}
