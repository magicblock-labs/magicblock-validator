use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

pub fn trigger_commit_cpi_instruction(
    payer: Pubkey,
    recvr: Pubkey,
    program_id: Pubkey,
) -> Instruction {
    let instruction_data = vec![0];
    let account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(recvr, false),
        AccountMeta::new(program_id, false),
    ];
    let program_id = crate::id();
    Instruction::new_with_bytes(program_id, &instruction_data, account_metas)
}
