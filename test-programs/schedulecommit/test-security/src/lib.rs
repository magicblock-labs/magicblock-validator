use schedulecommit_program::{
    api::schedule_commit_cpi_instruction, create_schedule_commit_ix,
    process_schedulecommit_cpi,
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
        // This instruction attempts to commit twice as follows:
        //
        // a) via the program owning the PDAs
        // b) directly via the MagicBlock program schedule commit
        //
        // We use it to see what the invoke contexts look like in this case and
        // to related it prepare for a similar case where the instruction to the
        // PDA program is any other instruction that does not commit.
        //
        // # Account references
        // - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
        // - **1**    `[]`              MagicBlock Program (used to schedule commit)
        // - **2..n** `[]`              The ScheduleCommit program
        // - **3..n** `[]`              PDA accounts to be committed
        //
        // # Instruction Args
        //
        // - **0..32**   Player 1 pubkey from which first PDA was derived
        // - **32..64**  Player 2 pubkey from which second PDA was derived
        // - **n..n+32** Player n pubkey from which n-th PDA was derived
        0 => process_sibling_schedule_cpis(accounts, instruction_data_inner)?,

        // Just an instruction to process without any CPI into any other program
        // - **0.**   `[WRITE, SIGNER]` Payer
        1 => process_non_cpi(accounts, instruction_data_inner)?,

        // This instruction attempts to commit the CPI directly via MagicBlock program,
        // however this only works if it is also the owner of the PDAs.
        // It is reusing the instruction or the _legit_ program (owning the PDAs)
        // The only difference is that it is a different program owning the invoked
        // instruction.
        // We also don't attempt to modify the PDA accounts since we do not own them.
        //
        // # Account references
        // - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
        // - **1**    `[]`              MagicBlock Program (used to schedule commit)
        // - **2..n** `[]`              PDA accounts to be committed
        //
        // # Instruction Args
        //
        // - **0..32**   Player 1 pubkey from which first PDA was derived
        // - **32..64**  Player 2 pubkey from which second PDA was derived
        // - **n..n+32** Player n pubkey from which n-th PDA was derived
        2 => process_schedulecommit_cpi(
            accounts,
            instruction_data_inner,
            false,
            false,
        )?,
        discriminant => {
            msg!("Error: unknown instruction: [{}]", discriminant);
            msg!("Instruction data: {:?}", instruction_data);
        }
    }
    Ok(())
}

fn process_sibling_schedule_cpis(
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    msg!("Processing sibling_cpis instruction");

    let accounts_iter = &mut accounts.iter();
    let payer = next_account_info(accounts_iter)?;
    let magic_program = next_account_info(accounts_iter)?;
    // Passed to us to allow CPI into it
    let _schedule_commmit_program = next_account_info(accounts_iter);

    let accounts_iter = &mut accounts.iter();

    let mut pda_infos = vec![];
    for info in accounts_iter.by_ref().skip(3) {
        pda_infos.push(info.clone());
    }
    let account_infos = vec![payer];

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
        account_infos.extend(pda_infos.iter());

        let direct_ix = create_schedule_commit_ix(
            *magic_program.key,
            &account_infos.to_vec(),
            false,
        );
        invoke(
            &direct_ix,
            &account_infos.into_iter().cloned().collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}

fn process_non_cpi(
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    msg!("Processing non_cpi instruction");
    msg!("Accounts: {}", accounts.len());
    msg!("Instruction data: {:?}", instruction_data);

    Ok(())
}
