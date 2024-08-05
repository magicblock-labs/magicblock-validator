use std::str::FromStr;

use sleipnir_core::magic_program::MAGIC_PROGRAM_ADDR;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

fn create_wrapper_schedule_cpis_instruction(
    payer: Pubkey,
    pdas: &[Pubkey],
    player_pubkeys: &[Pubkey],
    ix_id: u8,
) -> Instruction {
    let magic_program = Pubkey::from_str(MAGIC_PROGRAM_ADDR).unwrap();
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new_readonly(magic_program, false),
        AccountMeta::new_readonly(schedulecommit_program::id(), false),
    ];

    let mut instruction_data = vec![ix_id];
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

pub fn create_sibling_schedule_cpis_instruction(
    payer: Pubkey,
    pdas: &[Pubkey],
    player_pubkeys: &[Pubkey],
) -> Instruction {
    create_wrapper_schedule_cpis_instruction(payer, pdas, player_pubkeys, 0)
}

pub fn create_sibling_non_cpi_instruction(payer: Pubkey) -> Instruction {
    let account_metas = vec![AccountMeta::new(payer, true)];
    let instruction_data = vec![1, 0, 0, 0];
    Instruction::new_with_bytes(
        schedulecommit_security::id(),
        &instruction_data,
        account_metas,
    )
}

pub fn create_nested_schedule_cpis_instruction(
    payer: Pubkey,
    pdas: &[Pubkey],
    player_pubkeys: &[Pubkey],
) -> Instruction {
    create_wrapper_schedule_cpis_instruction(payer, pdas, player_pubkeys, 2)
}
