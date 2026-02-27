use borsh::to_vec;
use ephemeral_rollups_sdk::{
    ephem::{
        CallHandler, CommitAndUndelegate, CommitType, MagicAction,
        MagicInstructionBuilder, MagicIntentBundleBuilder, UndelegateType,
    },
    ActionArgs, ShortAccountMeta,
};
use solana_program::{
    account_info::{next_account_info, next_account_infos, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
};

use crate::instruction::FlexiCounterInstruction;

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
    let commit_action =
        FlexiCounterInstruction::CommitActionHandler { amount: PRIZE };
    let call_handlers = committees
        .iter()
        .zip(escrow_authorities.iter().cloned())
        .map(|(committee, escrow_authority)| {
            let other_accounts = vec![
                // counter account
                committee.into(),
                ShortAccountMeta {
                    pubkey: *transfer_destination.key,
                    is_writable: true,
                },
                system_program.into(),
            ];

            CallHandler {
                args: ActionArgs {
                    data: to_vec(&commit_action).unwrap(),
                    escrow_index: ACTOR_ESCROW_INDEX,
                },
                compute_units,
                escrow_authority,
                destination_program: *destination_program.key,
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
                let undelegate_action =
                    FlexiCounterInstruction::UndelegateActionHandler {
                        counter_diff,
                        amount: PRIZE,
                    };

                let other_accounts = vec![
                    // counter account
                    committee.into(),
                    ShortAccountMeta {
                        pubkey: *transfer_destination.key,
                        is_writable: true,
                    },
                    system_program.into(),
                ];

                Ok(CallHandler {
                    args: ActionArgs {
                        data: to_vec(&undelegate_action).unwrap(),
                        escrow_index: ACTOR_ESCROW_INDEX,
                    },
                    compute_units,
                    escrow_authority,
                    destination_program: *destination_program.key,
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

/// Process CreateIntentBundle instruction that creates an IntentBundle containing
/// both Commit and CommitAndUndelegate intents simultaneously using MagicIntentBundleBuilder.
///
/// This tests the new SDK feature where a single bundle can contain multiple intent types.
pub fn process_create_intent_bundle(
    accounts: &[AccountInfo],
    num_commit_only: u8,
    num_undelegate: u8,
    counter_diffs: Vec<i64>,
    compute_units: u32,
) -> ProgramResult {
    msg!(
        "Process create intent bundle: {} commit-only, {} undelegate",
        num_commit_only,
        num_undelegate
    );

    let num_commit_only = num_commit_only as usize;
    let num_undelegate = num_undelegate as usize;

    // Expected accounts:
    // 5 fixed + 2*num_commit_only (escrow + counter) + 2*num_undelegate (escrow + counter)
    let expected_accounts = 5 + 2 * num_commit_only + 2 * num_undelegate;
    let actual_accounts = accounts.len();
    if actual_accounts != expected_accounts {
        msg!(
            "Invalid number of accounts expected: {}, got: {}",
            expected_accounts,
            actual_accounts
        );
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let account_info_iter = &mut accounts.iter();

    // Fixed accounts
    let destination_program = next_account_info(account_info_iter)?;
    let magic_context = next_account_info(account_info_iter)?;
    let magic_program = next_account_info(account_info_iter)?;
    let transfer_destination = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;

    // Commit-only accounts
    let commit_only_escrows =
        next_account_infos(account_info_iter, num_commit_only)?;
    let commit_only_counters =
        next_account_infos(account_info_iter, num_commit_only)?;

    // CommitAndUndelegate accounts
    let undelegate_escrows =
        next_account_infos(account_info_iter, num_undelegate)?;
    let undelegate_counters =
        next_account_infos(account_info_iter, num_undelegate)?;

    // Get the first available payer for the builder
    let payer = if !commit_only_escrows.is_empty() {
        commit_only_escrows[0].clone()
    } else if !undelegate_escrows.is_empty() {
        undelegate_escrows[0].clone()
    } else {
        msg!("No payers provided");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Start building the intent bundle
    let mut builder = MagicIntentBundleBuilder::new(
        payer,
        magic_context.clone(),
        magic_program.clone(),
    );

    // Build Commit intent (commit-only accounts)
    if !commit_only_counters.is_empty() {
        let commit_action =
            FlexiCounterInstruction::CommitActionHandler { amount: PRIZE };
        let call_handlers = commit_only_counters
            .iter()
            .zip(commit_only_escrows.iter().cloned())
            .map(|(counter, escrow_authority)| {
                let other_accounts = vec![
                    counter.into(),
                    ShortAccountMeta {
                        pubkey: *transfer_destination.key,
                        is_writable: true,
                    },
                    system_program.into(),
                ];

                CallHandler {
                    args: ActionArgs {
                        data: to_vec(&commit_action).unwrap(),
                        escrow_index: ACTOR_ESCROW_INDEX,
                    },
                    compute_units,
                    escrow_authority,
                    destination_program: *destination_program.key,
                    accounts: other_accounts,
                }
            })
            .collect::<Vec<_>>();

        builder = builder
            .commit(commit_only_counters)
            .add_post_commit_actions(call_handlers)
            .fold();
    }

    // Build CommitAndUndelegate intent
    if !undelegate_counters.is_empty() {
        // Post-commit actions for CommitAndUndelegate
        let commit_action =
            FlexiCounterInstruction::CommitActionHandler { amount: PRIZE };
        let commit_handlers = undelegate_counters
            .iter()
            .zip(undelegate_escrows.iter().cloned())
            .map(|(counter, escrow_authority)| {
                let other_accounts = vec![
                    counter.into(),
                    ShortAccountMeta {
                        pubkey: *transfer_destination.key,
                        is_writable: true,
                    },
                    system_program.into(),
                ];

                CallHandler {
                    args: ActionArgs {
                        data: to_vec(&commit_action).unwrap(),
                        escrow_index: ACTOR_ESCROW_INDEX,
                    },
                    compute_units,
                    escrow_authority,
                    destination_program: *destination_program.key,
                    accounts: other_accounts,
                }
            })
            .collect::<Vec<_>>();

        // Post-undelegate actions
        let undelegate_handlers = undelegate_counters
            .iter()
            .zip(undelegate_escrows.iter().cloned())
            .zip(counter_diffs.iter().copied())
            .map(|((counter, escrow_authority), counter_diff)| {
                let undelegate_action =
                    FlexiCounterInstruction::UndelegateActionHandler {
                        counter_diff,
                        amount: PRIZE,
                    };

                let other_accounts = vec![
                    counter.into(),
                    ShortAccountMeta {
                        pubkey: *transfer_destination.key,
                        is_writable: true,
                    },
                    system_program.into(),
                ];

                CallHandler {
                    args: ActionArgs {
                        data: to_vec(&undelegate_action).unwrap(),
                        escrow_index: ACTOR_ESCROW_INDEX,
                    },
                    compute_units,
                    escrow_authority,
                    destination_program: *destination_program.key,
                    accounts: other_accounts,
                }
            })
            .collect::<Vec<_>>();

        builder = builder
            .commit_and_undelegate(undelegate_counters)
            .add_post_commit_actions(commit_handlers)
            .add_post_undelegate_actions(undelegate_handlers)
            .fold();
    }

    // Build and invoke the intent bundle
    builder.build_and_invoke()
}
