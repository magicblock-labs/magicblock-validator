use std::slice::Iter;

use borsh::{to_vec, BorshDeserialize};
use ephemeral_rollups_sdk::{
    cpi::{delegate_account, DelegateAccounts, DelegateConfig},
    pda::ephemeral_balance_pda_from_payer,
    CallHandlerArgs, Context,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    program::invoke,
    program_error::ProgramError,
    system_instruction::transfer,
};

use crate::{
    args::{CallHandlerDiscriminator, CommitActionData, UndelegateActionData},
    state::FlexiCounter,
};

pub fn process_call_handler(
    accounts: &[AccountInfo],
    call_data: &[u8],
) -> ProgramResult {
    msg!("Call handler");

    let mut account_info_iter = accounts.iter();
    let escrow_authority = next_account_info(&mut account_info_iter)?;
    let escrow_account = next_account_info(&mut account_info_iter)?;

    let mut call_handler = CallHandlerArgs::try_from_slice(call_data)?;
    let discriminator = &call_handler.data.as_slice()[0..4];
    if discriminator == CallHandlerDiscriminator::Simple.to_array() {
        call_handler.data.drain(0..4);
        process_simple_call_handler(
            escrow_authority,
            escrow_account,
            account_info_iter,
            &call_handler,
        )
    } else if discriminator == CallHandlerDiscriminator::ReDelegate.to_array()
    {
        call_handler.data.drain(0..4);
        process_redelegation_call_handler(
            escrow_authority,
            escrow_account,
            account_info_iter,
            &call_handler,
        )
    } else {
        Err(ProgramError::InvalidArgument)
    }
}

fn process_simple_call_handler<'a, 'b>(
    escrow_authority: &AccountInfo<'a>,
    escrow_account: &AccountInfo<'a>,
    mut account_info_iter: Iter<'b, AccountInfo<'a>>,
    call_handler: &CallHandlerArgs,
) -> ProgramResult
where
    'a: 'b,
{
    msg!("Simple call handler");

    let expected_escrow = ephemeral_balance_pda_from_payer(
        escrow_authority.key,
        call_handler.escrow_index,
    );
    if &expected_escrow != escrow_account.key {
        msg!("Escrow mismatch");
        return Err(ProgramError::InvalidAccountData);
    }
    if !escrow_account.is_signer {
        msg!("Escrow account shall be a signer");
        return Err(ProgramError::MissingRequiredSignature);
    }

    let account_info_iter = &mut account_info_iter;
    let delegated_account = next_account_info(account_info_iter)?;
    let transfer_destination = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;
    match call_handler.context {
        Context::Commit => {
            msg!("Commit context");
            if delegated_account.owner != &ephemeral_rollups_sdk::id() {
                msg!("account not owned by dlp");
                return Err(ProgramError::InvalidAccountOwner);
            }

            let commit_action_data =
                CommitActionData::try_from_slice(&call_handler.data)?;
            invoke(
                &transfer(
                    escrow_account.key,
                    transfer_destination.key,
                    commit_action_data.transfer_amount,
                ),
                &[
                    escrow_account.clone(),
                    transfer_destination.clone(),
                    system_program.clone(),
                ],
            )
        }
        Context::Undelegate => {
            msg!("Undelegate context");
            if delegated_account.owner == &ephemeral_rollups_sdk::id() {
                msg!("account still owned by dlp!");
                return Err(ProgramError::InvalidAccountOwner);
            }

            let undelegation_action_data =
                UndelegateActionData::try_from_slice(&call_handler.data)?;

            let mut counter =
                FlexiCounter::try_from_slice(&delegated_account.data.borrow())?;

            counter.count = (counter.count as i64
                + undelegation_action_data.counter_diff)
                as u64;
            counter.updates += 1;

            let size = delegated_account.data_len();
            let counter_data = to_vec(&counter)?;
            delegated_account.data.borrow_mut()[..size]
                .copy_from_slice(&counter_data);

            invoke(
                &transfer(
                    escrow_account.key,
                    transfer_destination.key,
                    undelegation_action_data.transfer_amount,
                ),
                &[
                    escrow_account.clone(),
                    transfer_destination.clone(),
                    system_program.clone(),
                ],
            )
        }
        Context::Standalone => {
            msg!("Standalone");
            Ok(())
        }
    }
}

fn process_redelegation_call_handler<'a, 'b>(
    escrow_authority: &AccountInfo<'a>,
    escrow: &AccountInfo<'a>,
    mut account_info_iter: Iter<'b, AccountInfo<'a>>,
    call_handler: &CallHandlerArgs,
) -> ProgramResult
where
    'a: 'b,
{
    msg!("Redelegation call handler");

    let account_info_iter = &mut account_info_iter;
    let delegated_account = next_account_info(account_info_iter)?;
    let destination_program = next_account_info(account_info_iter)?;
    let delegated_buffer = next_account_info(account_info_iter)?;
    let delegation_record = next_account_info(account_info_iter)?;
    let delegation_metadata = next_account_info(account_info_iter)?;
    let delegation_program = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;

    let Context::Undelegate = call_handler.context else {
        return Err(ProgramError::InvalidArgument);
    };

    // In our case escrow authority is creator, this could be handled in many other way
    let seeds_no_bump = FlexiCounter::seeds(escrow_authority.key);
    delegate_account(
        DelegateAccounts {
            payer: escrow,
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
