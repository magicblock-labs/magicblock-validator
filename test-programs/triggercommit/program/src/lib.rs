use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::ProgramResult,
    msg,
    native_token::LAMPORTS_PER_SOL,
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
    let recvr = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;

    msg!("payer: {:?}", payer);
    let amount = LAMPORTS_PER_SOL;
    invoke_signed(
        &system_instruction::transfer(payer.key, recvr.key, amount),
        &[payer.clone(), recvr.clone(), system_program.clone()],
        &[&[&payer.key.to_bytes()[..]]],
    )?;

    /*
        invoke_signed(
            &solana_program::system_instruction::transfer(
                payer.key,
                &Pubkey::new_unique(),
                1,
            ),
            &[payer.clone()],
            &[&[&payer.key.to_bytes()[..]]],
        )?;
    */

    Ok(())
}
