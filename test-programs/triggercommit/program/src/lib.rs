use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    msg,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    pubkey::Pubkey,
    system_instruction,
};
pub mod api;

declare_id!("9hgprgZiRWmy8KkfvUuaVkDGrqo9GzeXMohwq6BazgUY");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let (instruction_discriminant, instruction_data_inner) =
        instruction_data.split_at(1);
    match instruction_discriminant[0] {
        0 => {
            process_triggercommit_cpi(accounts, instruction_data_inner)?;
        }
        _ => {
            msg!("Error: unknown instruction")
        }
    }
    Ok(())
}

pub fn process_triggercommit_cpi(
    accounts: &[AccountInfo],
    _instruction_data: &[u8],
) -> Result<(), ProgramError> {
    msg!("Processing triggercommit_cpi instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let committee = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;

    let ix = create_trigger_commit_ix(
        *magic_program.key,
        *payer.key,
        *committee.key,
    );
    invoke(
        &ix,
        &[payer.clone(), committee.clone(), magic_program.clone()],
        // &[&[&payer.key.to_bytes()[..]]],
    )?;

    Ok(())
}

fn create_trigger_commit_ix(
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
