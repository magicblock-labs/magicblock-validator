use crate::args::{CommitActionData, UndelegateActionData};
use borsh::to_vec;
use ephemeral_rollups_sdk::ephem::{
    CallHandler, CommitAndUndelegate, CommitType, MagicAction,
    MagicInstructionBuilder, UndelegateType,
};
use ephemeral_rollups_sdk::ActionArgs;
use solana_program::account_info::{next_account_info, AccountInfo};
use solana_program::entrypoint::ProgramResult;
use solana_program::msg;
use solana_program::program_error::ProgramError;

pub const ACTOR_ESCROW_INDEX: u8 = 1;

pub fn process_create_intent(
    accounts: &[AccountInfo],
    counter_diff: i64,
    is_undelegate: bool,
    compute_units: u32,
) -> ProgramResult {
    const PRIZE: u64 = 1_000_000;

    msg!("Process create intent!");
    let account_info_iter = &mut accounts.iter();

    let payer = next_account_info(account_info_iter)?;
    let counter = next_account_info(account_info_iter)?;
    let destination_program = next_account_info(account_info_iter)?;
    let magic_context = next_account_info(account_info_iter)?;
    let magic_program = next_account_info(account_info_iter)?;

    if !payer.is_signer {
        msg!("escrow authority required to sign tx");
        return Err(ProgramError::MissingRequiredSignature);
    }

    let commit_action = CommitActionData {
        transfer_amount: PRIZE,
    };
    let other_accounts = vec![
        // counter account
        counter.clone(),
        // transfer destination from escrow
        next_account_info(account_info_iter)?.clone(),
        // system_program
        next_account_info(account_info_iter)?.clone(),
    ];

    let commit_type_action = CommitType::WithHandler {
        commited_accounts: vec![counter.clone()],
        call_handlers: vec![CallHandler {
            args: ActionArgs {
                data: to_vec(&commit_action)?,
                escrow_index: ACTOR_ESCROW_INDEX,
            },
            compute_untis: compute_units,
            escrow_authority: payer.clone(),
            destination_program: destination_program.clone(),
            accounts: other_accounts.clone(),
        }],
    };

    let magic_action = if is_undelegate {
        let undelegate_action_data = UndelegateActionData {
            counter_diff,
            transfer_amount: PRIZE,
        };
        let undelegate_action =
            UndelegateType::WithHandler(vec![CallHandler {
                args: ActionArgs {
                    data: to_vec(&undelegate_action_data)?,
                    escrow_index: ACTOR_ESCROW_INDEX,
                },
                compute_untis: compute_units,
                escrow_authority: payer.clone(),
                destination_program: destination_program.clone(),
                accounts: other_accounts,
            }]);
        let undelegate_type_action = CommitAndUndelegate {
            commit_type: commit_type_action,
            undelegate_type: undelegate_action,
        };
        MagicAction::CommitAndUndelegate(undelegate_type_action)
    } else {
        MagicAction::Commit(commit_type_action)
    };

    msg!("calling magic!");
    MagicInstructionBuilder {
        payer: payer.clone(),
        magic_context: magic_context.clone(),
        magic_program: magic_program.clone(),
        magic_action,
    }
    .build_and_invoke()
}
