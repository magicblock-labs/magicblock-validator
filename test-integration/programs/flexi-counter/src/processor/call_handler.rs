use crate::args::{CommitActionData, UndelegateActionData};
use crate::state::FlexiCounter;
use borsh::{to_vec, BorshDeserialize};
use ephemeral_rollups_sdk::pda::ephemeral_balance_pda_from_payer;
use ephemeral_rollups_sdk::{CallHandlerArgs, Context};
use solana_program::account_info::{next_account_info, AccountInfo};
use solana_program::entrypoint::ProgramResult;
use solana_program::msg;
use solana_program::program::invoke;
use solana_program::program_error::ProgramError;
use solana_program::system_instruction::transfer;

pub fn process_call_handler(
    accounts: &[AccountInfo],
    call_data: &[u8],
) -> ProgramResult {
    msg!("Call handler");
    let account_info_iter = &mut accounts.iter();
    let escrow_authority = next_account_info(account_info_iter)?;
    let escrow_account = next_account_info(account_info_iter)?;

    let call_handler = CallHandlerArgs::try_from_slice(call_data)?;
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
