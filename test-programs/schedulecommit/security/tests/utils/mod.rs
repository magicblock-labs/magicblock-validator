use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

pub fn create_sibling_schedule_cpis_instruction(
    payer: Pubkey,
    pda_owning_program_id: Pubkey,
    validator_id: Pubkey,
    magic_program_id: Pubkey,
    pdas: &[Pubkey],
    player_pubkeys: &[Pubkey],
) -> Instruction {
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(pda_owning_program_id, false),
        AccountMeta::new(validator_id, false),
        AccountMeta::new_readonly(magic_program_id, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    let mut instruction_data = vec![0];
    for pubkey in pdas {
        account_metas.push(AccountMeta {
            pubkey: *pubkey,
            is_signer: false,
            // NOTE: It appears they need to be writable to be properly cloned?
            is_writable: true,
        });
    }
    for pubkey in player_pubkeys {
        instruction_data.extend_from_slice(&pubkey.to_bytes());
    }
    Instruction::new_with_bytes(
        schedulecommit_security::id(),
        &instruction_data,
        account_metas,
    )
}
