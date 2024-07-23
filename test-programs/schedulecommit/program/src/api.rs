use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

pub fn init_account_instruction(
    payer: Pubkey,
    committee: Pubkey,
) -> Instruction {
    let program_id = crate::id();
    let account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(committee, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    let instruction_data = vec![0];
    Instruction::new_with_bytes(program_id, &instruction_data, account_metas)
}

pub fn schedule_commit_cpi_instruction(
    payer: Pubkey,
    validator_id: Pubkey,
    magic_program_id: Pubkey,
    committees: &[Pubkey],
) -> Instruction {
    let program_id = crate::id();
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(program_id, false),
        AccountMeta::new(validator_id, false),
        AccountMeta::new_readonly(magic_program_id, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    for committee in committees {
        account_metas.push(AccountMeta::new_readonly(*committee, false));
    }

    let instruction_data = vec![1];
    Instruction::new_with_bytes(program_id, &instruction_data, account_metas)
}

// -----------------
// PDA
// -----------------
const ACCOUNT: &str = "magic_schedule_commit";

pub fn pda_seeds(acc_id: &Pubkey) -> [&[u8]; 2] {
    [ACCOUNT.as_bytes(), acc_id.as_ref()]
}

pub fn pda_seeds_with_bump<'a>(
    acc_id: &'a Pubkey,
    bump: &'a [u8; 1],
) -> [&'a [u8]; 3] {
    [ACCOUNT.as_bytes(), acc_id.as_ref(), bump]
}

pub fn pda_and_bump(acc_id: &Pubkey) -> (Pubkey, u8) {
    let program_id = crate::id();
    let seeds = pda_seeds(acc_id);
    Pubkey::find_program_address(&seeds, &program_id)
}
