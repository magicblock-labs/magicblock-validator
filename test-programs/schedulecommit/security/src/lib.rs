use schedulecommit_program::{
    api::schedule_commit_cpi_instruction, create_schedule_commit_ix,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    pubkey::Pubkey,
};
use solana_sdk::program::invoke;

declare_id!("AeLmXCbPaQHGWRLr2saFsEVfmMNuKnxRAbWCT9P5twgz");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction<'a>(
    program_id: &'a Pubkey,
    accounts: &'a [AccountInfo<'a>],
    instruction_data: &[u8],
) -> ProgramResult {
    let (instruction_discriminant, instruction_data_inner) =
        instruction_data.split_at(1);
    match instruction_discriminant[0] {
        0 => process_sibling_cpis(accounts, instruction_data_inner)?,
        _ => {
            msg!("Error: unknown instruction")
        }
    }
    Ok(())
}

fn process_sibling_cpis(
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    msg!("Processing sibling_cpis instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let owning_program = next_account_info(accounts_iter)?;
    let validator_auth = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;
    let system_program = next_account_info(accounts_iter)?;

    let mut remaining = vec![];
    for info in accounts_iter.by_ref() {
        let mut x = info.clone();
        x.is_signer = true;
        remaining.push(x);
    }
    let mut account_infos =
        vec![payer, owning_program, validator_auth, system_program];
    account_infos.extend(remaining.iter());

    // 1. CPI into the program owning the PDAs
    msg!("Creating schedule commit CPI");
    let players = instruction_data
        .chunks(32)
        .map(|x| Pubkey::try_from(x).unwrap())
        .collect::<Vec<_>>();
    msg!("Players: {:?}", players);
    let pdas = account_infos.iter().map(|x| *x.key).collect::<Vec<_>>();
    msg!("PDAs: {:?}", pdas);

    let indirect_ix = schedule_commit_cpi_instruction(
        *payer.key,
        *validator_auth.key,
        *magic_program.key,
        &players,
        &pdas,
    );
    let cloned_account_infos = account_infos
        .clone()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    invoke(&indirect_ix, &cloned_account_infos)?;

    // 2. CPI into the schedule commit directly
    let direct_ix =
        create_schedule_commit_ix(*magic_program.key, &account_infos);
    invoke(&direct_ix, &cloned_account_infos)?;
    Ok(())
}
