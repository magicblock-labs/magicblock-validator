use borsh::{to_vec, BorshDeserialize};
use ephemeral_rollups_sdk::cpi::{
    delegate_account, DelegateAccounts, DelegateConfig,
};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program::invoke,
    program_error::ProgramError, system_instruction::transfer,
};

use crate::state::FlexiCounter;

pub fn process_commit_action_handler(
    accounts: &[AccountInfo],
    amount: u64,
) -> ProgramResult {
    msg!("CommitActionHandler");

    let [_, escrow_account, delegated_account, destination_account, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !escrow_account.is_signer {
        msg!("Escrow account shall be a signer");
        return Err(ProgramError::MissingRequiredSignature);
    }

    // During commit, delegated account must still be owned by ER.
    if delegated_account.owner != &ephemeral_rollups_sdk::id() {
        msg!("account not owned by ER (dlp)");
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Transfer from escrow to destination.
    invoke(
        &transfer(escrow_account.key, destination_account.key, amount),
        &[
            escrow_account.clone(),
            destination_account.clone(),
            system_program.clone(),
        ],
    )
}

pub fn process_undelegate_action_handler(
    accounts: &[AccountInfo],
    amount: u64,
    counter_diff: i64,
) -> ProgramResult {
    msg!("UndelegateActionHandler");

    let [_, escrow_account, undelegated_counter, destination_account, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !escrow_account.is_signer {
        msg!("Escrow account shall be a signer");
        return Err(ProgramError::MissingRequiredSignature);
    }

    // After undelegation, delegated account must NOT be owned by ER anymore.
    if undelegated_counter.owner == &ephemeral_rollups_sdk::id() {
        msg!("account still owned by ER (dlp)!");
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Update counter
    {
        let mut counter = {
            let data = undelegated_counter.data.borrow();
            FlexiCounter::deserialize(&mut data.as_ref())?
        };

        counter.count = u64::try_from(counter.count as i64 + counter_diff)
            .map_err(|_| ProgramError::ArithmeticOverflow)?;
        counter.updates += 1;

        let counter_data = to_vec(&counter)?;
        undelegated_counter.data.borrow_mut()[..counter_data.len()]
            .copy_from_slice(&counter_data);
    }

    // Transfer from escrow to destination.
    invoke(
        &transfer(escrow_account.key, destination_account.key, amount),
        &[
            escrow_account.clone(),
            destination_account.clone(),
            system_program.clone(),
        ],
    )
}

// NOTE: due to prohibited reentrancy in solana this isn't possible for now
// Issue: dlp calls User program, User program calls delegate in dlp
#[allow(dead_code)]
fn process_redelegation_call_handler<'a, 'b>(
    accounts: &[AccountInfo],
) -> ProgramResult
where
    'a: 'b,
{
    msg!("Redelegation call handler");

    let [escrow_authority, escrow_account, delegated_account, destination_program, delegated_buffer, delegation_record, delegation_metadata, delegation_program, system_program] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // In our case escrow authority is creator, this could be handled in many other way
    let seeds_no_bump = FlexiCounter::seeds(escrow_authority.key);
    delegate_account(
        DelegateAccounts {
            payer: escrow_account,
            pda: delegated_account,
            owner_program: destination_program,
            buffer: delegated_buffer,
            delegation_record,
            delegation_metadata,
            delegation_program,
            system_program,
        },
        &seeds_no_bump,
        // Could be passed in CallHandlerArgs::data
        DelegateConfig {
            commit_frequency_ms: 1000,
            validator: None,
        },
    )?;

    Ok(())
}
