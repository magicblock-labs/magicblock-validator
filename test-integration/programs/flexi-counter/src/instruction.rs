use crate::state::FlexiCounter;
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum FlexiCounterInstruction {
    /// Creates a FlexiCounter account.
    ///
    /// Accounts:
    /// 0. `[signer]` The payer that is creating the account.
    /// 1. `[write]` The counter PDA account that will be created.
    /// 2. `[]` The system program account.
    Init { label: String, bump: u8 },
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
