use ephemeral_rollups_sdk::{
    ephem::{
        CallHandler, CommitAndUndelegate, CommitType, MagicAction,
        MagicInstructionBuilder, UndelegateType,
    },
    ActionArgs,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
};

use crate::args::CallHandlerDiscriminator;

pub const ACTOR_ESCROW_INDEX: u8 = 1;

/// Can't be used as for now. Awaiting PR with custom AccountMeta overwrites
pub fn process_create_redelegation_intent(
    accounts: &[AccountInfo],
) -> ProgramResult {
    msg!("Creating redelegation intent");

    let account_info_iter = &mut accounts.iter();
    // Accounts for redelegation
    let escrow_authority = next_account_info(account_info_iter)?;
    let delegated_account = next_account_info(account_info_iter)?;
    let destination_program = next_account_info(account_info_iter)?;
    let delegated_buffer = next_account_info(account_info_iter)?;
    let delegation_record = next_account_info(account_info_iter)?;
    let delegation_metadata = next_account_info(account_info_iter)?;
    let delegation_program = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;

    // Our special accounts
    let magic_context = next_account_info(account_info_iter)?;
    let magic_program = next_account_info(account_info_iter)?;

    // Set proper writable data
    let other_accounts = vec![
        // Undelegated account at that point
        delegated_account.clone(),
        // Payer is escrow an included by dlp::call_handler
        // .. ,
        // Owner
        destination_program.clone(),
        // records and such
        delegated_buffer.clone(),
        delegation_record.clone(),
        delegation_metadata.clone(),
        delegation_program.clone(),
        system_program.clone(),
    ];

    let call_handler = CallHandler {
        args: ActionArgs {
            data: [CallHandlerDiscriminator::ReDelegate.to_vec()].concat(),
            escrow_index: ACTOR_ESCROW_INDEX,
        },
        compute_units: 150_000,
        escrow_authority: escrow_authority.clone(),
        destination_program: destination_program.clone(),
        accounts: other_accounts,
    };

    let magic_action = MagicAction::CommitAndUndelegate(CommitAndUndelegate {
        commit_type: CommitType::Standalone(vec![delegated_account.clone()]),
        undelegate_type: UndelegateType::WithHandler(vec![call_handler]),
    });

    MagicInstructionBuilder {
        payer: escrow_authority.clone(),
        magic_context: magic_context.clone(),
        magic_program: magic_program.clone(),
        magic_action,
    }
    .build_and_invoke()
}
