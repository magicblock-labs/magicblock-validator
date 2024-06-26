use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

pub fn trigger_commit(
    magic_program_id: Pubkey,
    payer: Pubkey,
    committee: Pubkey,
) -> Instruction {
    let instruction_data = vec![1, 0, 0, 0];
    let account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(committee, false),
    ];
    Instruction::new_with_bytes(
        magic_program_id,
        &instruction_data,
        account_metas,
    )
}
