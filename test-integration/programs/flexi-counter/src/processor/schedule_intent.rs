use crate::args::{CommitActionData, UndelegateActionData};
use borsh::to_vec;
use ephemeral_rollups_sdk::ephem::{
    CallHandler, CommitAndUndelegate, CommitType, MagicAction,
    MagicInstructionBuilder, UndelegateType,
};
use ephemeral_rollups_sdk::ActionArgs;
use solana_program::account_info::{
    next_account_info, next_account_infos, AccountInfo,
};
use solana_program::entrypoint::ProgramResult;
use solana_program::msg;
use solana_program::program_error::ProgramError;

pub const ACTOR_ESCROW_INDEX: u8 = 1;
const PRIZE: u64 = 1_000_000;

pub fn process_create_intent(
    accounts: &[AccountInfo],
    num_committees: u8,
    counter_diffs: Vec<i64>,
    is_undelegate: bool,
    compute_units: u32,
) -> ProgramResult {
    msg!("Process create intent for {} committees!", num_committees);

    let num_committees = num_committees as usize;
    let expected_accounts = 2 * num_committees + 5;
    let actual_accounts = accounts.len();
    if accounts.len() != 2 * num_committees + 5 {
        msg!(
            "Invalid number of accounts expected: {}, got: {}",
            expected_accounts,
            actual_accounts
        );
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let account_info_iter = &mut accounts.iter();

    let destination_program = next_account_info(account_info_iter)?;
    let magic_context = next_account_info(account_info_iter)?;
    let magic_program = next_account_info(account_info_iter)?;
    // other accounts
    let transfer_destination = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;

    let escrow_authorities =
        next_account_infos(account_info_iter, num_committees)?;
    let committees = next_account_infos(account_info_iter, num_committees)?;

    // Create commit actions
    let commit_action = CommitActionData {
        transfer_amount: PRIZE,
    };
    let mut commit_action_data = to_vec(&commit_action)?;
    commit_action_data.extend_from_slice(&[1; 3]);
    commit_action_data[2] = 1;
    commit_action_data[3] = 0;
    let call_handlers = committees
        .iter()
        .zip(escrow_authorities.iter().cloned())
        .map(|(committee, escrow_authority)| {
            let other_accounts = vec![
                // counter account
                committee.clone(),
                transfer_destination.clone(),
                system_program.clone(),
            ];

            CallHandler {
                args: ActionArgs {
                    data: commit_action_data.clone(),
                    escrow_index: ACTOR_ESCROW_INDEX,
                },
                compute_untis: compute_units,
                escrow_authority,
                destination_program: destination_program.clone(),
                accounts: other_accounts,
            }
        })
        .collect::<Vec<_>>();
    let commit_action = CommitType::WithHandler {
        commited_accounts: committees.to_vec(),
        call_handlers,
    };

    let magic_action = if is_undelegate {
        let call_handlers = committees
            .iter()
            .zip(escrow_authorities.iter().cloned())
            .zip(counter_diffs.iter().copied())
            .map(|((committee, escrow_authority), counter_diff)| {
                let undelegate_action_data = UndelegateActionData {
                    counter_diff,
                    transfer_amount: PRIZE,
                };

                let other_accounts = vec![
                    // counter account
                    committee.clone(),
                    transfer_destination.clone(),
                    system_program.clone(),
                ];

                Ok(CallHandler {
                    args: ActionArgs {
                        data: to_vec(&undelegate_action_data)?,
                        escrow_index: ACTOR_ESCROW_INDEX,
                    },
                    compute_untis: compute_units,
                    escrow_authority,
                    destination_program: destination_program.clone(),
                    accounts: other_accounts,
                })
            })
            .collect::<Result<Vec<_>, ProgramError>>()?;
        let undelegate_action = UndelegateType::WithHandler(call_handlers);
        let undelegate_type_action = CommitAndUndelegate {
            commit_type: commit_action,
            undelegate_type: undelegate_action,
        };
        MagicAction::CommitAndUndelegate(undelegate_type_action)
    } else {
        MagicAction::Commit(commit_action)
    };

    MagicInstructionBuilder {
        payer: escrow_authorities[0].clone(),
        magic_context: magic_context.clone(),
        magic_program: magic_program.clone(),
        magic_action,
    }
    .build_and_invoke()
}
