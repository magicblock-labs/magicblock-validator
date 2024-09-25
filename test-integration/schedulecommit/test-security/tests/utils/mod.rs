use integration_test_tools::conversions::pubkey_from_magic_program;
use sleipnir_core::magic_program;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

/// Attempts to commit the PDAs twice as follows:
/// - via the program owning the PDAs
/// - directly via the MagicBlock program schedule commit
pub fn create_sibling_schedule_cpis_instruction(
    payer: Pubkey,
    pdas: &[Pubkey],
    player_pubkeys: &[Pubkey],
) -> Instruction {
    let magic_program = pubkey_from_magic_program(magic_program::id());
    let magic_context =
        pubkey_from_magic_program(magic_program::MAGIC_CONTEXT_PUBKEY);
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(magic_context, false),
        AccountMeta::new_readonly(magic_program, false),
        AccountMeta::new_readonly(schedulecommit_program::id(), false),
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
        schedulecommit_test_security::id(),
        &instruction_data,
        account_metas,
    )
}

/// Attempts to commit the CPI directly via MagicBlock program, but should fail since
/// it is not the owner of the PDAs it is committing.
pub fn create_nested_schedule_cpis_instruction(
    payer: Pubkey,
    pdas: &[Pubkey],
    player_pubkeys: &[Pubkey],
) -> Instruction {
    let magic_program = pubkey_from_magic_program(magic_program::id());
    let magic_context =
        pubkey_from_magic_program(magic_program::MAGIC_CONTEXT_PUBKEY);
    let mut account_metas = vec![
        AccountMeta::new(payer, true),
        AccountMeta::new(magic_context, false),
        AccountMeta::new_readonly(magic_program, false),
    ];

    let mut instruction_data = vec![2];
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
        schedulecommit_test_security::id(),
        &instruction_data,
        account_metas,
    )
}

/// Creates basically a noop instruction that does nothing.
/// It could be added to confuse our algorithm to detect the invoking program.
pub fn create_sibling_non_cpi_instruction(payer: Pubkey) -> Instruction {
    let account_metas = vec![AccountMeta::new(payer, true)];
    let instruction_data = vec![1, 0, 0, 0];
    Instruction::new_with_bytes(
        schedulecommit_test_security::id(),
        &instruction_data,
        account_metas,
    )
}
