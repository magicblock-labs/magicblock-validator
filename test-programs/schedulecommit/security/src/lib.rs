use schedulecommit_program::{
    api::schedule_commit_cpi_instruction, create_schedule_commit_ix,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    declare_id,
    entrypoint::ProgramResult,
    msg,
    program::invoke,
    pubkey::Pubkey,
};

declare_id!("4RaQH3CUBMSMQsSHPVaww2ifeNEEuaDZjF9CUdFwr3xr");

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction<'a>(
    _program_id: &'a Pubkey,
    accounts: &'a [AccountInfo<'a>],
    instruction_data: &[u8],
) -> ProgramResult {
    let (instruction_discriminant, instruction_data_inner) =
        instruction_data.split_at(1);
    match instruction_discriminant[0] {
        // # Account references
        // - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
        // - **1.**   `[SIGNER]`        The program owning the accounts to be committed
        // - **2.**   `[WRITE]`         Validator authority to which we escrow tx cost
        // - **3**    `[]`              MagicBlock Program (used to schedule commit)
        // - **4**    `[]`              System Program to support PDA signing
        // - **5..n** `[]`              PDA accounts to be committed
        //
        // # Instruction Args
        //
        // - **0..32**   Player 1 pubkey from which first PDA was derived
        // - **32..64**  Player 2 pubkey from which second PDA was derived
        // - **n..n+32** Player n pubkey from which n-th PDA was derived
        0 => process_sibling_schedule_cpis(accounts, instruction_data_inner)?,
        _ => {
            msg!("Error: unknown instruction")
        }
    }
    Ok(())
}

/// This instruction attempts to commit twice as follows:
///
/// a) via the program owning the PDAs
/// b) directly into the schedule commit
///
/// We use it to see what the invoke contexts look like in this case and
/// to related it prepare for a similar case where the instruction to the
/// PDA program is any other instruction that does not commit.
fn process_sibling_schedule_cpis(
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

    let accounts_iter = &mut accounts.iter();

    let mut pda_infos = vec![];
    for info in accounts_iter.by_ref().skip(5) {
        let mut x = info.clone();
        x.is_signer = true;
        pda_infos.push(x);
    }
    let account_infos =
        vec![payer, owning_program, validator_auth, system_program];

    msg!("Creating schedule commit CPI");
    let players = instruction_data
        .chunks(32)
        .map(|x| Pubkey::try_from(x).unwrap())
        .collect::<Vec<_>>();
    let pdas = pda_infos.iter().map(|x| *x.key).collect::<Vec<_>>();

    msg!("Players: {:?}", players);
    msg!("PDAs: {:?}", pdas);

    {
        // 1. CPI into the program owning the PDAs
        let indirect_ix = schedule_commit_cpi_instruction(
            *payer.key,
            *validator_auth.key,
            *magic_program.key,
            &players,
            &pdas,
        );
        let mut account_infos = account_infos
            .clone()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        account_infos.extend(pda_infos.clone());
        invoke(&indirect_ix, &account_infos)?;
    }

    {
        // 2. CPI into the schedule commit directly
        let mut account_infos = account_infos.clone();
        let non_signer_pda_infos = pda_infos
            .iter()
            .map(|x| {
                let mut y = x.clone();
                y.is_signer = false;
                y
            })
            .collect::<Vec<_>>();
        account_infos.extend(non_signer_pda_infos.iter());

        let direct_ix = create_schedule_commit_ix(
            *magic_program.key,
            &account_infos.to_vec(),
        );
        invoke(
            &direct_ix,
            &account_infos.into_iter().cloned().collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}
