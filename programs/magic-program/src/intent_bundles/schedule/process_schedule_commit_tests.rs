use std::collections::HashMap;

use assert_matches::assert_matches;
use magic_program_api::{
    MAGIC_CONTEXT_PUBKEY, OUTBOX_INTENT_PROGRAM_ID,
    args::{
        ActionArgs, AddActionCallbackArgs, BaseActionArgs, MagicIntentBundleArgs, ShortAccountMeta,
    },
    instruction::MagicBlockInstruction,
};
use mbv_core::intent::{
    ACTUAL_COMMIT_LIMIT, COMMIT_FEE_LAMPORTS, outbox::outbox_intent_pda_with_bump,
};
use solana_account::{
    AccountBuilder, AccountMode, AccountSharedData, ReadableAccount, WritableAccount,
    create_account_shared_data_for_test,
};
use solana_clock::Clock;
use solana_fee_calculator::DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE;
use solana_instruction::{AccountMeta, Instruction, error::InstructionError};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_sdk_ids::{system_program, sysvar::clock};
use solana_signer::Signer;

use crate::{
    magic_context::MagicContext,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent::outbox_intent_bundles::OutboxIntentBundle,
    schedule_transactions::COMMIT_LIMIT,
    schedule_transactions::magic_fee_vault_pubkey,
    test_utils::{
        StubNonces, ensure_authority, ensure_started_validator, process_instruction,
        process_instruction_with_logs,
    },
    utils::DELEGATION_PROGRAM_ID,
};

type AccountDataMap = HashMap<Pubkey, AccountSharedData>;
type TransactionAccounts = Vec<(Pubkey, AccountSharedData)>;
type PreparedScheduleAccounts = (AccountDataMap, TransactionAccounts, Option<Pubkey>);

// For the scheduling itself and the debit to fund the scheduled transaction
const REQUIRED_TX_COST: u64 = DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE * 2;

/// Delegation is a mode rather than a flag, so `false` here means the account
/// is simply not delegated: an ordinary readonly account.
fn mode_for(delegated: bool) -> AccountMode {
    if delegated { AccountMode::Delegated } else { AccountMode::Mirrored }
}

fn get_clock() -> Clock {
    Clock {
        slot: 100,
        unix_timestamp: 1_000,
        epoch_start_timestamp: 0,
        epoch: 10,
        leader_schedule_epoch: 10,
    }
}

fn action_only_bundle_args(
    action_account: Pubkey,
    destination_program: Pubkey,
) -> MagicIntentBundleArgs {
    MagicIntentBundleArgs {
        commit: None,
        commit_and_undelegate: None,
        commit_finalize: None,
        commit_finalize_and_undelegate: None,
        standalone_actions: vec![BaseActionArgs {
            args: ActionArgs::new(vec![1, 2, 3]).with_escrow_index(0),
            compute_units: 100_000,
            escrow_authority: 0,
            destination_program,
            accounts: vec![ShortAccountMeta {
                pubkey: action_account,
                is_writable: true,
            }],
        }],
    }
}

fn transaction_accounts_with_clock() -> Vec<(Pubkey, AccountSharedData)> {
    vec![(
        clock::id(),
        create_account_shared_data_for_test(&get_clock()),
    )]
}

fn magic_context_account() -> AccountSharedData {
    AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id())
}

fn prepare_schedule_accounts(
    payer: &Keypair,
    payer_delegated: bool,
    payer_confined: bool,
    fee_vault_delegated: Option<bool>,
) -> PreparedScheduleAccounts {
    let mut accounts_data = HashMap::new();
    let payer_acc = AccountBuilder::from(AccountSharedData::new(
        REQUIRED_TX_COST,
        0,
        &system_program::id(),
    ))
    .mode(if payer_confined { AccountMode::Magic } else { mode_for(payer_delegated) })
    .build();
    accounts_data.insert(payer.pubkey(), payer_acc);
    accounts_data.insert(MAGIC_CONTEXT_PUBKEY, magic_context_account());

    let fee_vault_pubkey = fee_vault_delegated.map(|is_delegated| {
        ensure_authority();
        let pubkey = magic_fee_vault_pubkey();
        let vault_acc = AccountBuilder::from(AccountSharedData::new(0, 0, &system_program::id()))
            .mode(mode_for(is_delegated))
            .build();
        accounts_data.insert(pubkey, vault_acc);
        pubkey
    });

    ensure_started_validator(&mut accounts_data, None);

    (
        accounts_data,
        transaction_accounts_with_clock(),
        fee_vault_pubkey,
    )
}

fn schedule_action_only_bundle_instruction(
    payer: &Pubkey,
    action_account: Pubkey,
    destination_program: Pubkey,
    fee_vault: Option<Pubkey>,
) -> Instruction {
    let mut account_metas =
        vec![AccountMeta::new(*payer, true), AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false)];
    if let Some(fee_vault) = fee_vault {
        account_metas.push(AccountMeta::new(fee_vault, false));
    }

    Instruction::new_with_wincode(
        crate::id(),
        &MagicBlockInstruction::ScheduleIntentBundle(action_only_bundle_args(
            action_account,
            destination_program,
        )),
        account_metas,
    )
}

fn add_action_callback_instruction(
    payer: &Pubkey,
    fee_vault: Option<Pubkey>,
    destination_program: Pubkey,
) -> Instruction {
    let mut account_metas =
        vec![AccountMeta::new(*payer, true), AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false)];
    if let Some(fee_vault) = fee_vault {
        account_metas.push(AccountMeta::new(fee_vault, false));
    }

    Instruction::new_with_wincode(
        crate::id(),
        &MagicBlockInstruction::AddActionCallback(AddActionCallbackArgs {
            action_index: 0,
            destination_program,
            discriminator: vec![],
            payload: vec![],
            compute_units: 0,
            accounts: vec![],
        }),
        account_metas,
    )
}

#[cfg(test)]
mod scheduling_cpi_tests {
    use std::sync::Arc;

    use solana_program_runtime::{
        declare_process_instruction, invoke_context::mock_process_instruction,
        loaded_programs::ProgramCacheEntry, solana_sbpf::program::BuiltinFunctionDefinition,
    };

    use super::*;
    use crate::magicblock_processor::Entrypoint;

    declare_process_instruction!(SchedulingProgram, 0, |invoke_context| {
        let instructions: Vec<Instruction> = {
            let context = invoke_context.transaction_context.get_current_instruction_context()?;
            wincode::deserialize(context.get_instruction_data())
                .map_err(|_| InstructionError::InvalidInstructionData)?
        };
        for instruction in instructions {
            invoke_context.native_invoke_signed(instruction, &[])?;
        }
        Ok(())
    });

    #[test]
    #[serial_test::serial]
    fn fee_bearing_actions_require_payment() {
        use magic_program_api::args::{
            CommitAndUndelegateArgs, CommitTypeArgs, MagicBaseIntentArgs, UndelegateTypeArgs,
        };

        let source = Pubkey::new_unique();
        let payer = Keypair::new();
        let committee = Pubkey::new_unique();
        // None means no actions; Some(0) keeps zero-fee actions distinct.
        for (mode, with_vault, units) in [
            (AccountMode::Mirrored, false, Some(100_000)),
            (AccountMode::Mirrored, true, Some(100_000)),
            (AccountMode::Magic, false, Some(100_000)),
            (AccountMode::Magic, true, Some(100_000)),
            (AccountMode::Delegated, true, Some(100_000)),
            (AccountMode::Delegated, false, Some(100_000)),
            (AccountMode::Mirrored, false, Some(1)),
            (AccountMode::Mirrored, false, Some(0)),
            (AccountMode::Mirrored, false, None),
        ] {
            let delegated = mode == AccountMode::Delegated;
            let committee_index = if with_vault { 3 } else { 2 };
            let mut actions = action_only_bundle_args(committee, source).standalone_actions;
            if let Some(units) = units {
                actions[0].compute_units = units;
            } else {
                actions.clear();
            }
            let commit = if actions.is_empty() {
                CommitTypeArgs::Standalone(vec![committee_index])
            } else {
                CommitTypeArgs::WithBaseActions {
                    committed_accounts: vec![committee_index],
                    base_actions: actions.clone(),
                }
            };
            let undelegate = CommitAndUndelegateArgs {
                commit_type: commit.clone(),
                undelegate_type: UndelegateTypeArgs::Standalone,
            };
            let mut intents = vec![
                MagicBaseIntentArgs::Commit(commit.clone()),
                MagicBaseIntentArgs::CommitFinalize(commit),
                MagicBaseIntentArgs::CommitAndUndelegate(undelegate.clone()),
                MagicBaseIntentArgs::CommitFinalizeAndUndelegate(undelegate),
            ];
            if !actions.is_empty() {
                let undelegate = CommitAndUndelegateArgs {
                    commit_type: CommitTypeArgs::Standalone(vec![committee_index]),
                    undelegate_type: UndelegateTypeArgs::WithBaseActions {
                        base_actions: actions.clone(),
                    },
                };
                intents.extend([
                    MagicBaseIntentArgs::BaseActions(actions),
                    MagicBaseIntentArgs::CommitAndUndelegate(undelegate.clone()),
                    MagicBaseIntentArgs::CommitFinalizeAndUndelegate(undelegate),
                ]);
            }
            for intent in intents {
                let has_commit = !matches!(intent, MagicBaseIntentArgs::BaseActions(_));
                let undelegate = matches!(
                    intent,
                    MagicBaseIntentArgs::CommitAndUndelegate(_)
                        | MagicBaseIntentArgs::CommitFinalizeAndUndelegate(_)
                );
                // Paid commits cross the fee threshold. Unpaid, action-free
                // undelegation must remain available past the sponsored limit.
                let nonce = if delegated || (units.is_none() && undelegate) {
                    ACTUAL_COMMIT_LIMIT
                } else {
                    0
                };
                for secure in [false, true] {
                    let (mut accounts, mut transaction_accounts, vault) = prepare_schedule_accounts(
                        &payer,
                        delegated,
                        mode == AccountMode::Magic,
                        with_vault.then_some(true),
                    );
                    accounts.get_mut(&payer.pubkey()).unwrap().set_lamports(3_000_000);
                    accounts.insert(
                        committee,
                        AccountBuilder::from(AccountSharedData::new(1_000_000, 0, &source))
                            .mode(AccountMode::Delegated)
                            .commit_seq(nonce as u32)
                            .build(),
                    );
                    let mut metas = vec![
                        AccountMeta::new(payer.pubkey(), true),
                        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
                    ];
                    if let Some(vault) = vault {
                        metas.push(AccountMeta::new(vault, false));
                    }
                    metas.push(AccountMeta::new(committee, false));
                    let instruction = if secure {
                        MagicBlockInstruction::ScheduleIntentBundle(intent.clone().into())
                    } else {
                        MagicBlockInstruction::ScheduleBaseIntent(intent.clone())
                    };
                    let instruction =
                        Instruction::new_with_wincode(crate::id(), &instruction, metas.clone());
                    metas.push(AccountMeta::new_readonly(crate::id(), false));
                    accounts.insert(
                        crate::id(),
                        AccountSharedData::new(0, 0, &solana_sdk_ids::native_loader::id()),
                    );
                    for meta in &metas {
                        transaction_accounts
                            .push((meta.pubkey, accounts.remove(&meta.pubkey).unwrap()));
                    }
                    let before = transaction_accounts.clone();
                    let expected = if delegated && !with_vault {
                        Err(InstructionError::MissingAccount)
                    } else if units.is_some_and(|units| units > 0) && !delegated {
                        Err(InstructionError::IllegalOwner)
                    } else {
                        Ok(())
                    };
                    let result = mock_process_instruction(
                        &source,
                        None,
                        &wincode::serialize(&vec![instruction]).unwrap(),
                        transaction_accounts,
                        metas,
                        expected.clone(),
                        (SchedulingProgram::vm, SchedulingProgram::codegen),
                        |context| {
                            context.program_cache_for_tx_batch.replenish(
                                crate::id(),
                                Arc::new(ProgramCacheEntry::new_builtin((
                                    Entrypoint::vm,
                                    Entrypoint::codegen,
                                ))),
                            );
                        },
                        |_| {},
                    );
                    if expected.is_err() {
                        for (index, (_, account)) in before.iter().enumerate() {
                            assert_eq!(
                                &result[index], account,
                                "rejected {intent:?}, payer {mode:?}, vault {with_vault}, secure {secure}"
                            );
                        }
                        continue;
                    }
                    let account = |key| {
                        &result[before.iter().position(|(pubkey, _)| *pubkey == key).unwrap()]
                    };
                    let fee = if delegated {
                        5_000 + if has_commit { COMMIT_FEE_LAMPORTS } else { 0 }
                    } else {
                        0
                    };
                    assert_eq!(account(payer.pubkey()).lamports(), 3_000_000 - fee);
                    if let Some(vault) = vault {
                        assert_eq!(account(vault).lamports(), fee);
                    }
                    let context =
                        MagicContext::deserialize(account(MAGIC_CONTEXT_PUBKEY).data()).unwrap();
                    assert_eq!(context.scheduled_base_intents.len(), 1);
                    assert_eq!(
                        u64::from(account(committee).commit_seq()),
                        nonce + u64::from(has_commit)
                    );
                    assert!(account(committee).is(if undelegate {
                        AccountMode::Undelegating
                    } else {
                        AccountMode::Delegated
                    }));
                }
            }
        }
    }

    /// Real CPI provenance permits A-to-A registration but rejects A-to-B,
    /// while the original base action remains free to target B.
    #[test]
    #[serial_test::serial]
    fn test_callback_destination_bound_to_recorded_source() {
        use magic_program_api::args::{
            CommitAndUndelegateArgs, CommitTypeArgs, UndelegateTypeArgs,
        };

        let source = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        for destination in [source, other] {
            let payer = Keypair::new();
            let (mut accounts, mut transaction_accounts, fee_vault) =
                prepare_schedule_accounts(&payer, true, false, Some(true));
            let payer_account = accounts.remove(&payer.pubkey()).unwrap();
            accounts.insert(
                payer.pubkey(),
                AccountBuilder::from(payer_account)
                    .lamports(3_000_000)
                    .commit_seq(ACTUAL_COMMIT_LIMIT as u32)
                    .build(),
            );
            let committee = Pubkey::new_unique();
            accounts.insert(
                committee,
                AccountBuilder::from(AccountSharedData::new(1_000_000, 0, &source))
                    .mode(AccountMode::Delegated)
                    .build(),
            );
            let mut args = action_only_bundle_args(Pubkey::new_unique(), other);
            let mut small_action = args.standalone_actions[0].clone();
            small_action.compute_units = 1;
            args.commit_finalize = Some(CommitTypeArgs::WithBaseActions {
                committed_accounts: vec![0],
                base_actions: vec![small_action.clone(), small_action.clone()],
            });
            args.commit_and_undelegate = Some(CommitAndUndelegateArgs {
                commit_type: CommitTypeArgs::Standalone(vec![3]),
                undelegate_type: UndelegateTypeArgs::WithBaseActions {
                    base_actions: vec![small_action],
                },
            });
            let schedule = Instruction::new_with_wincode(
                crate::id(),
                &MagicBlockInstruction::ScheduleIntentBundle(args),
                vec![
                    AccountMeta::new(payer.pubkey(), true),
                    AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
                    AccountMeta::new(fee_vault.unwrap(), false),
                    AccountMeta::new(committee, false),
                ],
            );
            let callback = add_action_callback_instruction(&payer.pubkey(), fee_vault, destination);
            let mut outer_accounts = schedule.accounts.clone();
            outer_accounts.push(AccountMeta::new_readonly(crate::id(), false));
            accounts.insert(
                crate::id(),
                AccountSharedData::new(0, 0, &solana_sdk_ids::native_loader::id()),
            );
            for meta in &outer_accounts {
                transaction_accounts.push((meta.pubkey, accounts.remove(&meta.pubkey).unwrap()));
            }
            let accepted = destination == source;
            let result = mock_process_instruction(
                &source,
                None,
                &wincode::serialize(&vec![schedule, callback]).unwrap(),
                transaction_accounts,
                outer_accounts,
                if accepted { Ok(()) } else { Err(InstructionError::InvalidInstructionData) },
                (SchedulingProgram::vm, SchedulingProgram::codegen),
                |context| {
                    context.program_cache_for_tx_batch.replenish(
                        crate::id(),
                        Arc::new(ProgramCacheEntry::new_builtin((
                            Entrypoint::vm,
                            Entrypoint::codegen,
                        ))),
                    );
                },
                |_| {},
            );
            let account = find_magic_context_account(&result).unwrap();
            let mut context = MagicContext::deserialize(account.data()).unwrap();
            let scheduled = &context.scheduled_base_intents[0];
            // The 100k-CU action costs 5,000; each group of tiny actions rounds
            // up to one lamport. The snapshot includes all scheduling fees.
            let expected_fee = COMMIT_FEE_LAMPORTS + 5_000 + 2;
            assert_eq!(scheduled.calculate_fee().unwrap(), expected_fee);
            let payer_snapshot = scheduled
                .get_all_committed_accounts()
                .into_iter()
                .find(|account| account.pubkey == payer.pubkey())
                .unwrap();
            assert_eq!(payer_snapshot.account.lamports, 3_000_000 - expected_fee);
            let action = context.scheduled_base_intents[0].intent_bundle.get_action_mut(0).unwrap();
            assert_eq!(action.source_program, Some(source));
            assert_eq!(action.destination_program, other);
            assert_eq!(
                action.callback.as_ref().map(|c| c.destination_program),
                accepted.then_some(source)
            );
        }
    }
}

fn prepare_transaction_with_single_committee(
    payer: &Keypair,
    program: Pubkey,
    committee: Pubkey,
) -> (
    HashMap<Pubkey, AccountSharedData>,
    Vec<(Pubkey, AccountSharedData)>,
) {
    let mut account_data = {
        let mut map = HashMap::new();
        map.insert(
            payer.pubkey(),
            AccountSharedData::new(REQUIRED_TX_COST, 0, &system_program::id()),
        );
        // NOTE: the magic context is initialized with these properties at
        // validator startup
        map.insert(
            MAGIC_CONTEXT_PUBKEY,
            AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
        );

        map.insert(
            committee,
            AccountBuilder::from(AccountSharedData::new(0, 0, &program))
                .mode(AccountMode::Delegated)
                .build(),
        );
        map
    };
    ensure_started_validator(&mut account_data, None);

    let transaction_accounts: Vec<(Pubkey, AccountSharedData)> = vec![(
        clock::id(),
        create_account_shared_data_for_test(&get_clock()),
    )];

    (account_data, transaction_accounts)
}

struct PreparedTransactionThreeCommittees {
    program: Pubkey,
    accounts_data: HashMap<Pubkey, AccountSharedData>,
    committee_uno: Pubkey,
    committee_dos: Pubkey,
    committee_tres: Pubkey,
    transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
}

fn prepare_transaction_with_three_committees(
    payer: &Keypair,
    committees: Option<(Pubkey, Pubkey, Pubkey)>,
    is_delegated: (bool, bool, bool),
) -> PreparedTransactionThreeCommittees {
    let program = Pubkey::new_unique();
    let (committee_uno, committee_dos, committee_tres) = committees.unwrap_or((
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    ));

    let mut accounts_data = {
        let mut map = HashMap::new();
        map.insert(
            payer.pubkey(),
            AccountSharedData::new(REQUIRED_TX_COST, 0, &system_program::id()),
        );
        map.insert(
            MAGIC_CONTEXT_PUBKEY,
            AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
        );
        map.insert(
            committee_uno,
            AccountBuilder::from(AccountSharedData::new(0, 0, &program))
                .mode(mode_for(is_delegated.0))
                .build(),
        );
        map.insert(
            committee_dos,
            AccountBuilder::from(AccountSharedData::new(0, 0, &program))
                .mode(mode_for(is_delegated.1))
                .build(),
        );
        map.insert(
            committee_tres,
            AccountBuilder::from(AccountSharedData::new(0, 0, &program))
                .mode(mode_for(is_delegated.2))
                .build(),
        );
        map
    };
    ensure_started_validator(&mut accounts_data, None);

    let transaction_accounts: Vec<(Pubkey, AccountSharedData)> = vec![(
        clock::id(),
        create_account_shared_data_for_test(&get_clock()),
    )];

    PreparedTransactionThreeCommittees {
        program,
        accounts_data,
        committee_uno,
        committee_dos,
        committee_tres,
        transaction_accounts,
    }
}

fn find_magic_context_account(accounts: &[AccountSharedData]) -> Option<&AccountSharedData> {
    accounts
        .iter()
        .find(|acc| acc.owner() == &crate::id() && acc.lamports() == u64::MAX)
}

fn remove_magic_context_account(accounts: &mut Vec<AccountSharedData>) -> AccountSharedData {
    let index = accounts
        .iter()
        .position(|acc| acc.owner() == &crate::id() && acc.lamports() == u64::MAX)
        .expect("magic context account not found");
    accounts.remove(index)
}

fn assert_non_accepted_actions(
    processed_scheduled: &[AccountSharedData],
    expected_non_accepted_commits: usize,
) -> &AccountSharedData {
    let magic_context_acc =
        find_magic_context_account(processed_scheduled).expect("magic context account not found");
    let magic_context = MagicContext::deserialize(magic_context_acc.data()).unwrap();

    assert_eq!(
        magic_context.scheduled_base_intents.len(),
        expected_non_accepted_commits
    );

    magic_context_acc
}

fn assert_accepted_actions(
    processed_accepted: &[AccountSharedData],
    pre_accept_magic_context: &AccountSharedData,
    expected_accepted_count: usize,
) -> Vec<ScheduledIntentBundle> {
    let post_magic_context_acc =
        find_magic_context_account(processed_accepted).expect("magic context account not found");
    let post_magic_context = MagicContext::deserialize(post_magic_context_acc.data()).unwrap();
    assert_eq!(post_magic_context.scheduled_base_intents.len(), 0);

    let pre_magic_context = MagicContext::deserialize(pre_accept_magic_context.data()).unwrap();
    let accepted_intents = pre_magic_context.scheduled_base_intents;
    assert_eq!(accepted_intents.len(), expected_accepted_count);

    for intent in &accepted_intents {
        let bump = outbox_intent_pda_with_bump(intent.intent_id).1;
        let expected = OutboxIntentBundle::accepted(intent.clone(), bump);
        let actual = processed_accepted
            .iter()
            .filter(|acc| acc.owner() == &OUTBOX_INTENT_PROGRAM_ID && acc.is(AccountMode::Magic))
            .filter_map(|acc| OutboxIntentBundle::try_from_bytes(acc.data()).ok())
            .find(|bundle| bundle.inner.intent_id == intent.intent_id)
            .unwrap_or_else(|| {
                panic!(
                    "outbox PDA for intent {} not found in processed_accepted",
                    intent.intent_id
                )
            });
        assert_eq!(actual, expected);
    }

    accepted_intents
}

/// Pre-populates uninitialized outbox intent PDA accounts into `account_data`.
/// The accept instruction creates these accounts via CPI (`CreateOutboxIntent`),
/// which requires them to be present in the transaction context as uninitialized
/// system-owned entries — exactly what `validate_new_pda` expects.
/// The first 3 accounts (validator, outbox program, magic_context) are
/// already in `account_data`, so we skip them.
fn ensure_outbox_pda_accounts_exist(
    account_data: &mut HashMap<Pubkey, AccountSharedData>,
    accept_ix: &Instruction,
) {
    for acc_meta in accept_ix.accounts.iter().skip(3) {
        account_data
            .entry(acc_meta.pubkey)
            .or_insert_with(|| AccountSharedData::new(0, 0, &system_program::id()));
    }
}

fn extend_transaction_accounts_from_ix(
    ix: &Instruction,
    account_data: &mut HashMap<Pubkey, AccountSharedData>,
    transaction_accounts: &mut Vec<(Pubkey, AccountSharedData)>,
) {
    transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
        account_data.remove(&acc.pubkey).map(|shared_data| (acc.pubkey, shared_data))
    }));
}

fn extend_transaction_accounts_from_ix_adding_magic_context(
    ix: &Instruction,
    magic_context_acc: &AccountSharedData,
    account_data: &mut HashMap<Pubkey, AccountSharedData>,
    transaction_accounts: &mut Vec<(Pubkey, AccountSharedData)>,
) {
    transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
        account_data.remove(&acc.pubkey).map(|shared_data| {
            let shared_data = if acc.pubkey == MAGIC_CONTEXT_PUBKEY {
                magic_context_acc.clone()
            } else {
                shared_data
            };
            (acc.pubkey, shared_data)
        })
    }));
}

fn assert_first_commit(
    scheduled_base_intents: &[ScheduledIntentBundle],
    payer: &Pubkey,
    committees: &[Pubkey],
    expected_request_undelegation: bool,
) {
    let scheduled_base_intent = &scheduled_base_intents[0];
    let test_clock = get_clock();
    assert_matches!(
        scheduled_base_intent,
        ScheduledIntentBundle {
            intent_id: id,
            slot,
            payer: actual_payer,
            blockhash: _,
            intent_bundle,
            ..
        } => {
            assert!(id >= &0);
            assert_eq!(slot, &test_clock.slot);
            assert_eq!(actual_payer, payer);
            assert_eq!(intent_bundle.get_all_committed_pubkeys().as_slice(), committees);
            assert!(intent_bundle.commit.is_none());
            assert!(intent_bundle.commit_and_undelegate.is_none());
            if expected_request_undelegation {
                assert!(intent_bundle.commit_finalize.is_none());
                assert!(intent_bundle.commit_finalize_and_undelegate.is_some());
            } else {
                assert!(intent_bundle.commit_finalize.is_some());
                assert!(intent_bundle.commit_finalize_and_undelegate.is_none());
            }
            assert_eq!(intent_bundle.has_undelegate_intent(), expected_request_undelegation);
        }
    );
}

#[cfg(test)]
mod tests {
    // ---------- Helpers for ATA/eATA remapping tests ----------
    // Use shared SPL/ATA/eATA constants and helpers
    // Reuse test helper to create proper SPL ATA account data
    use chainlink::testing::eatas::{create_ata_account, create_token_2022_ata_account};
    use mbv_core::token_programs::{
        EATA_PROGRAM_ID, MAGIC_ATA_CLOSE_AUTHORITY, TOKEN_2022_PROGRAM_ID, derive_ata,
        derive_ata_with_token_program, derive_eata,
    };
    use serial_test::serial;
    use solana_program::{program_option::COption, program_pack::Pack};
    use solana_seed_derivable::SeedDerivable;
    use spl_token::state::Account as SplAccount;

    use super::*;
    use crate::{utils::instruction_utils::InstructionUtils, validator};

    fn make_delegated_spl_ata_account(owner: &Pubkey, mint: &Pubkey) -> AccountSharedData {
        AccountBuilder::from(create_ata_account(owner, mint))
            .mode(AccountMode::Delegated)
            .build()
    }

    fn make_delegated_token_2022_ata_account(owner: &Pubkey, mint: &Pubkey) -> AccountSharedData {
        AccountBuilder::from(create_token_2022_ata_account(owner, mint))
            .mode(AccountMode::Delegated)
            .build()
    }

    fn make_magic_spl_ata_account(owner: &Pubkey, mint: &Pubkey) -> AccountSharedData {
        let mut acc = make_delegated_spl_ata_account(owner, mint);
        let mut token = SplAccount::unpack(acc.data()).unwrap();
        token.close_authority = COption::Some(MAGIC_ATA_CLOSE_AUTHORITY);
        SplAccount::pack(token, acc.data_as_mut_slice()).unwrap();
        AccountBuilder::from(acc).mode(AccountMode::Magic).build()
    }

    #[test]
    #[serial]
    fn test_schedule_commit_single_account_success() {
        let payer = Keypair::from_seed(b"schedule_commit_single_account_success").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (processed_scheduled, magic_context_acc) = {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(&payer, program, committee);

            let ix =
                InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![committee]);

            extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

            let mut processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            assert_non_accepted_actions(&processed_scheduled, 1);
            let magic_context_acc = remove_magic_context_account(&mut processed_scheduled);

            (processed_scheduled, magic_context_acc)
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(&payer, program, committee);

            let intent_ids = MagicContext::deserialize(magic_context_acc.data())
                .unwrap()
                .scheduled_base_intents
                .into_iter()
                .map(|i| i.intent_id)
                .collect::<Vec<_>>();
            let ix = InstructionUtils::accept_scheduled_commits_instruction(
                validator::authority(),
                intent_ids.into_iter(),
            );
            ensure_outbox_pda_accounts_exist(&mut account_data, &ix);
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut account_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_intents =
                assert_accepted_actions(&processed_accepted, &magic_context_acc, 1);

            assert_first_commit(&scheduled_intents, &payer.pubkey(), &[committee], false);
        }
        let committed_account = processed_scheduled.last().unwrap();
        assert_eq!(*committed_account.owner(), program);
    }

    #[test]
    #[serial]
    fn test_schedule_intent_bundle_action_only_rejects_missing_caller() {
        let payer = Keypair::from_seed(b"schedule_intent_bundle_action_only_two_accounts").unwrap();
        let action_account = Pubkey::new_unique();
        let destination_program = Pubkey::new_unique();

        let (mut accounts_data, mut transaction_accounts, _) =
            prepare_schedule_accounts(&payer, false, false, None);
        let ix = schedule_action_only_bundle_instruction(
            &payer.pubkey(),
            action_account,
            destination_program,
            None,
        );

        extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::UnsupportedProgramId),
        );
    }

    #[test]
    #[serial]
    /// An optional vault must still be delegated when the payer is not charged.
    fn test_schedule_commit_optional_fee_vault_is_validated() {
        let payer = Keypair::from_seed(&[33u8; 32]).unwrap();
        let (mut accounts_data, mut transaction_accounts, fee_vault) =
            prepare_schedule_accounts(&payer, false, false, Some(false));
        // Exercise the shared vault checker without bypassing bundle CPI provenance.
        let ix = instruction_from_account_metas(vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            AccountMeta::new(fee_vault.unwrap(), false),
        ]);

        extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IllegalOwner),
        );
    }

    #[test]
    #[serial]
    /// A delegated callback payer cannot omit the fee vault.
    fn test_add_action_callback_requires_fee_vault() {
        let payer = Keypair::from_seed(&[34u8; 32]).unwrap();

        let (mut accounts_data, mut transaction_accounts, _) =
            prepare_schedule_accounts(&payer, true, false, None);
        let ix = add_action_callback_instruction(&payer.pubkey(), None, Pubkey::new_unique());

        extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingAccount),
        );
    }

    #[test]
    #[serial]
    /// An ephemeral payer cannot charge the vault even when it is supplied.
    fn test_add_action_callback_confined_payer_cannot_charge_fee_vault() {
        let payer = Keypair::from_seed(&[35u8; 32]).unwrap();
        let (mut accounts_data, mut transaction_accounts, fee_vault) =
            prepare_schedule_accounts(&payer, true, true, Some(true));
        let ix = add_action_callback_instruction(&payer.pubkey(), fee_vault, Pubkey::new_unique());

        extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingAccount),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_single_account_and_request_undelegate_success() {
        let payer = Keypair::from_seed(b"single_account_with_undelegate_success").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (processed_scheduled, magic_context_acc) = {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(&payer, program, committee);

            let ix = InstructionUtils::schedule_commit_and_undelegate_instruction(
                &payer.pubkey(),
                vec![committee],
            );

            extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

            let mut processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            assert_non_accepted_actions(&processed_scheduled, 1);
            let magic_context_acc = remove_magic_context_account(&mut processed_scheduled);

            (processed_scheduled, magic_context_acc)
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(&payer, program, committee);

            let intent_ids = MagicContext::deserialize(magic_context_acc.data())
                .unwrap()
                .scheduled_base_intents
                .into_iter()
                .map(|i| i.intent_id)
                .collect::<Vec<_>>();
            let ix = InstructionUtils::accept_scheduled_commits_instruction(
                validator::authority(),
                intent_ids.into_iter(),
            );
            ensure_outbox_pda_accounts_exist(&mut account_data, &ix);
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut account_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_commits =
                assert_accepted_actions(&processed_accepted, &magic_context_acc, 1);

            assert_first_commit(&scheduled_commits, &payer.pubkey(), &[committee], true);
        }
        let committed_account = processed_scheduled.last().unwrap();
        assert_eq!(*committed_account.owner(), DELEGATION_PROGRAM_ID);
    }

    #[test]
    #[serial]
    fn test_schedule_commit_remaps_delegated_ata_to_eata() {
        let payer = Keypair::from_seed(b"schedule_commit_remap_ata_to_eata").unwrap();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata_pubkey = derive_ata(&wallet_owner, &mint);
        let eata_pubkey = derive_eata(&wallet_owner, &mint);

        // 1) Prepare transaction with our ATA as the only committee
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, Pubkey::new_unique(), ata_pubkey);

        // Replace the committee account with a delegated SPL-Token ATA layout
        account_data.insert(
            ata_pubkey,
            make_delegated_spl_ata_account(&wallet_owner, &mint),
        );

        // Build ScheduleCommit instruction using the ATA pubkey
        let ix = InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![ata_pubkey]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        // Execute scheduling
        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        // Extract magic context and then accept scheduled commits
        let magic_context_acc = assert_non_accepted_actions(&processed_scheduled, 1);

        let intent_ids = MagicContext::deserialize(magic_context_acc.data())
            .unwrap()
            .scheduled_base_intents
            .into_iter()
            .map(|i| i.intent_id)
            .collect::<Vec<_>>();
        let ix_accept = InstructionUtils::accept_scheduled_commits_instruction(
            validator::authority(),
            intent_ids.into_iter(),
        );
        let (mut account_data2, mut transaction_accounts2) =
            prepare_transaction_with_single_committee(&payer, Pubkey::new_unique(), ata_pubkey);
        ensure_outbox_pda_accounts_exist(&mut account_data2, &ix_accept);
        extend_transaction_accounts_from_ix_adding_magic_context(
            &ix_accept,
            magic_context_acc,
            &mut account_data2,
            &mut transaction_accounts2,
        );
        let processed_accepted = process_instruction(
            ix_accept.data.as_slice(),
            transaction_accounts2,
            ix_accept.accounts,
            Ok(()),
        );

        let scheduled = assert_accepted_actions(&processed_accepted, magic_context_acc, 1);
        // Verify the committed pubkey remapped to eATA
        assert_eq!(
            scheduled[0].intent_bundle.get_all_committed_pubkeys(),
            vec![eata_pubkey]
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_rejects_magic_ata() {
        let payer = Keypair::from_seed(b"schedule_commit_rejects_rent_pend").unwrap();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata_pubkey = derive_ata(&wallet_owner, &mint);

        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, Pubkey::new_unique(), ata_pubkey);

        // Magic ATAs are ER-only and must never be committed.
        account_data.insert(ata_pubkey, make_magic_spl_ata_account(&wallet_owner, &mint));

        let ix = InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![ata_pubkey]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidAccountData),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_rejects_token_2022_ata_without_eata_caller() {
        let payer = Keypair::from_seed(b"schedule_commit_token_2022_ata_eata_parent").unwrap();
        let eata_parent_owned_committee = Pubkey::new_unique();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata_pubkey =
            derive_ata_with_token_program(&wallet_owner, &mint, &TOKEN_2022_PROGRAM_ID);
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer,
                EATA_PROGRAM_ID,
                eata_parent_owned_committee,
            );
        account_data.insert(
            ata_pubkey,
            make_delegated_token_2022_ata_account(&wallet_owner, &mint),
        );

        let ix = instruction_from_account_metas(vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            AccountMeta::new_readonly(eata_parent_owned_committee, false),
            AccountMeta::new_readonly(ata_pubkey, false),
        ]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidInstructionData),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_and_undelegate_remaps_delegated_ata_to_eata() {
        let payer = Keypair::from_seed(b"schedule_commit_undelegate_remap_ata_eata").unwrap();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata_pubkey = derive_ata(&wallet_owner, &mint);
        let eata_pubkey = derive_eata(&wallet_owner, &mint);

        // 1) Prepare transaction with our ATA as the only committee
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, Pubkey::new_unique(), ata_pubkey);

        // Replace the committee account with a delegated SPL-Token ATA layout
        account_data.insert(
            ata_pubkey,
            make_delegated_spl_ata_account(&wallet_owner, &mint),
        );

        // Build ScheduleCommitAndUndelegate instruction using the ATA pubkey (writable)
        let ix = InstructionUtils::schedule_commit_and_undelegate_instruction(
            &payer.pubkey(),
            vec![ata_pubkey],
        );
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        // Execute scheduling
        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        // Extract magic context and then accept scheduled commits
        let magic_context_acc = assert_non_accepted_actions(&processed_scheduled, 1);

        let intent_ids = MagicContext::deserialize(magic_context_acc.data())
            .unwrap()
            .scheduled_base_intents
            .into_iter()
            .map(|i| i.intent_id)
            .collect::<Vec<_>>();
        let ix_accept = InstructionUtils::accept_scheduled_commits_instruction(
            validator::authority(),
            intent_ids.into_iter(),
        );
        let (mut account_data2, mut transaction_accounts2) =
            prepare_transaction_with_single_committee(&payer, Pubkey::new_unique(), ata_pubkey);
        ensure_outbox_pda_accounts_exist(&mut account_data2, &ix_accept);
        extend_transaction_accounts_from_ix_adding_magic_context(
            &ix_accept,
            magic_context_acc,
            &mut account_data2,
            &mut transaction_accounts2,
        );
        let processed_accepted = process_instruction(
            ix_accept.data.as_slice(),
            transaction_accounts2,
            ix_accept.accounts,
            Ok(()),
        );

        let scheduled = assert_accepted_actions(&processed_accepted, magic_context_acc, 1);
        // Verify the committed pubkey remapped to eATA
        assert_eq!(
            scheduled[0].intent_bundle.get_all_committed_pubkeys(),
            vec![eata_pubkey]
        );
        // And the intent contains undelegation
        assert!(scheduled[0].intent_bundle.has_undelegate_intent());
    }

    #[test]
    #[serial]
    fn test_schedule_commit_three_accounts_success() {
        let payer = Keypair::from_seed(b"schedule_commit_three_accounts_success").unwrap();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (
            mut processed_scheduled,
            magic_context_acc,
            program,
            committee_uno,
            committee_dos,
            committee_tres,
        ) = {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                committee_uno,
                committee_dos,
                committee_tres,
                mut transaction_accounts,
                program,
                ..
            } = prepare_transaction_with_three_committees(&payer, None, (true, true, true));

            let ix = InstructionUtils::schedule_commit_instruction(
                &payer.pubkey(),
                vec![committee_uno, committee_dos, committee_tres],
            );
            extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

            let mut processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            assert_non_accepted_actions(&processed_scheduled, 1);
            let magic_context_acc = remove_magic_context_account(&mut processed_scheduled);

            (
                processed_scheduled,
                magic_context_acc,
                program,
                committee_uno,
                committee_dos,
                committee_tres,
            )
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                mut transaction_accounts,
                ..
            } = prepare_transaction_with_three_committees(
                &payer,
                Some((committee_uno, committee_dos, committee_tres)),
                (true, true, true),
            );

            let intent_ids = MagicContext::deserialize(magic_context_acc.data())
                .unwrap()
                .scheduled_base_intents
                .into_iter()
                .map(|i| i.intent_id)
                .collect::<Vec<_>>();
            let ix = InstructionUtils::accept_scheduled_commits_instruction(
                validator::authority(),
                intent_ids.into_iter(),
            );
            ensure_outbox_pda_accounts_exist(&mut accounts_data, &ix);
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut accounts_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_commits =
                assert_accepted_actions(&processed_accepted, &magic_context_acc, 1);

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
                &[committee_uno, committee_dos, committee_tres],
                false,
            );
            for _ in &[committee_uno, committee_dos, committee_tres] {
                let committed_account = processed_scheduled.pop().unwrap();
                assert_eq!(*committed_account.owner(), program);
            }
        }
    }

    #[test]
    #[serial]
    fn test_schedule_commit_three_accounts_and_request_undelegate_success() {
        let payer = Keypair::from_seed(b"three_accounts_and_request_undelegate_success").unwrap();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (
            mut processed_scheduled,
            magic_context_acc,
            _program,
            committee_uno,
            committee_dos,
            committee_tres,
        ) = {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                committee_uno,
                committee_dos,
                committee_tres,
                mut transaction_accounts,
                program,
                ..
            } = prepare_transaction_with_three_committees(&payer, None, (true, true, true));

            let ix = InstructionUtils::schedule_commit_and_undelegate_instruction(
                &payer.pubkey(),
                vec![committee_uno, committee_dos, committee_tres],
            );

            extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

            let mut processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            assert_non_accepted_actions(&processed_scheduled, 1);
            let magic_context_acc = remove_magic_context_account(&mut processed_scheduled);

            (
                processed_scheduled,
                magic_context_acc,
                program,
                committee_uno,
                committee_dos,
                committee_tres,
            )
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                mut transaction_accounts,
                ..
            } = prepare_transaction_with_three_committees(
                &payer,
                Some((committee_uno, committee_dos, committee_tres)),
                (true, true, true),
            );

            let intent_ids = MagicContext::deserialize(magic_context_acc.data())
                .unwrap()
                .scheduled_base_intents
                .into_iter()
                .map(|i| i.intent_id)
                .collect::<Vec<_>>();
            let ix = InstructionUtils::accept_scheduled_commits_instruction(
                validator::authority(),
                intent_ids.into_iter(),
            );
            ensure_outbox_pda_accounts_exist(&mut accounts_data, &ix);
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut accounts_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_commits =
                assert_accepted_actions(&processed_accepted, &magic_context_acc, 1);

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
                &[committee_uno, committee_dos, committee_tres],
                true,
            );
            for _ in &[committee_uno, committee_dos, committee_tres] {
                let committed_account = processed_scheduled.pop().unwrap();
                assert_eq!(*committed_account.owner(), DELEGATION_PROGRAM_ID);
            }
        }
    }

    // -----------------
    // Failure Cases
    // ----------------
    fn get_account_metas_for_schedule_commit(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas =
            vec![AccountMeta::new(*payer, true), AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false)];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new(*pubkey, true));
        }
        account_metas
    }

    fn account_metas_last_committee_not_signer(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas = get_account_metas_for_schedule_commit(payer, pdas);
        let last = account_metas.pop().unwrap();
        account_metas.push(AccountMeta::new_readonly(last.pubkey, false));
        account_metas
    }

    fn instruction_from_account_metas(account_metas: Vec<AccountMeta>) -> Instruction {
        Instruction::new_with_wincode(
            crate::id(),
            &MagicBlockInstruction::ScheduleCommit,
            account_metas,
        )
    }

    #[test]
    #[serial]
    fn test_schedule_commit_no_pdas_provided_to_ix() {
        let payer = Keypair::from_seed(b"schedule_commit_no_pdas_provided_to_ix").unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer, None, (true, true, true));

        let ix = instruction_from_account_metas(get_account_metas_for_schedule_commit(
            &payer.pubkey(),
            vec![],
        ));
        extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingAccount),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_undelegate_with_readonly() {
        let payer = Keypair::from_seed(b"schedule_commit_undelegate_with_readonly").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, program, committee);

        // Create ScheduleCommitAndUndelegate with committee as readonly account
        let ix = {
            let mut account_metas = vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            ];
            account_metas.push(AccountMeta::new_readonly(committee, true));
            Instruction::new_with_wincode(
                crate::id(),
                &MagicBlockInstruction::ScheduleCommitAndUndelegate,
                account_metas,
            )
        };

        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::ReadonlyDataModified),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_with_non_delegated_account() {
        let payer = Keypair::from_seed(b"schedule_commit_with_non_delegated_account").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // Prepare single accounts for tx, set committee as non delegated
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, program, committee);
        let committee_account = account_data.remove(&committee).unwrap();
        account_data.insert(
            committee,
            AccountBuilder::from(committee_account).mode(AccountMode::Mirrored).build(),
        );

        // Create ScheduleCommit instruction with non-delegated committee
        let ix = InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![committee]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IllegalOwner),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_three_accounts_second_not_owned_by_program_and_not_signer() {
        let payer = Keypair::from_seed(b"three_accounts_last_not_owned_by_program").unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer, None, (true, true, true));

        accounts_data.insert(
            committee_dos,
            AccountBuilder::from(AccountSharedData::new(0, 0, &Pubkey::new_unique()))
                .mode(AccountMode::Delegated)
                .build(),
        );

        let ix = instruction_from_account_metas(account_metas_last_committee_not_signer(
            &payer.pubkey(),
            vec![committee_uno, committee_tres, committee_dos],
        ));

        extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidInstructionData),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_with_confined_account() {
        let payer = Keypair::from_seed(b"schedule_commit_with_confined_account").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // Prepare single accounts for tx, set committee as confined
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, program, committee);
        let committee_account = account_data.remove(&committee).unwrap();
        account_data.insert(
            committee,
            AccountBuilder::from(committee_account).mode(AccountMode::Magic).build(),
        );

        let committee_account = account_data.get(&committee).unwrap();
        assert!(committee_account.is(AccountMode::Magic));

        // Create ScheduleCommit instruction with confined committee
        let ix = InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![committee]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidAccountData),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_fails_when_commit_limit_exceeded() {
        let payer = Keypair::from_seed(b"schedule_commit_limit_exceeded____").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, program, committee);

        // Seed account state at the commit limit
        ensure_started_validator(&mut account_data, Some(StubNonces::Global(COMMIT_LIMIT)));

        let ix = InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![committee]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::Custom(
                crate::schedule_transactions::COMMIT_LIMIT_ERR,
            )),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_logs_commit_limit_resolution() {
        let payer = Keypair::from_seed(b"schedule_commit_limit_log_msg___").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, program, committee);

        ensure_started_validator(&mut account_data, Some(StubNonces::Global(COMMIT_LIMIT)));

        let ix = InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![committee]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        let (_, logs) = process_instruction_with_logs(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::Custom(
                crate::schedule_transactions::COMMIT_LIMIT_ERR,
            )),
        );

        let expected_log = format!(
            "ScheduleCommit ERR: sponsored commit limit exceeded for account {}: reserved commit ID {} exceeds the limit of {}. Undelegate and re-delegate the account or use a delegated account as the payer",
            committee,
            COMMIT_LIMIT + 1,
            COMMIT_LIMIT
        );
        assert!(
            logs.iter().any(|log| log == &expected_log),
            "expected commit-limit log not found in {:?}",
            logs
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_and_undelegate_succeeds_when_commit_limit_exceeded() {
        let payer = Keypair::from_seed(b"undelegate_succeeds_limit_exceeded").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(&payer, program, committee);

        // Seed account state at the commit limit
        ensure_started_validator(&mut account_data, Some(StubNonces::Global(COMMIT_LIMIT)));

        let ix = InstructionUtils::schedule_commit_and_undelegate_instruction(
            &payer.pubkey(),
            vec![committee],
        );
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    #[serial]
    fn test_schedule_commit_three_accounts_one_confined() {
        let payer = Keypair::from_seed(b"three_accounts_one_confined_______").unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer, None, (true, true, true));

        // Make the second committee confined
        let committee_account = accounts_data.remove(&committee_dos).unwrap();
        accounts_data.insert(
            committee_dos,
            AccountBuilder::from(committee_account).mode(AccountMode::Magic).build(),
        );
        let committee_dos_account = accounts_data.get(&committee_dos).unwrap();
        assert!(
            committee_dos_account.is(AccountMode::Magic),
            "Confined account should remain confined"
        );

        let ix = InstructionUtils::schedule_commit_instruction(
            &payer.pubkey(),
            vec![committee_uno, committee_dos, committee_tres],
        );
        extend_transaction_accounts_from_ix(&ix, &mut accounts_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidAccountData),
        );
    }

    /// Helper: builds transaction accounts for a delegated-payer commit.
    /// Payer is delegated+writable; fee vault is delegated+writable.
    fn prepare_delegated_payer_transaction(
        payer: &Keypair,
        program: Pubkey,
        committees: &[Pubkey],
        nonces: StubNonces,
    ) -> (
        HashMap<Pubkey, AccountSharedData>,
        Vec<(Pubkey, AccountSharedData)>,
    ) {
        ensure_authority();
        let fee_vault_pubkey = magic_fee_vault_pubkey();

        let mut account_data = {
            let mut map = HashMap::new();

            map.insert(
                payer.pubkey(),
                AccountBuilder::from(AccountSharedData::new(1_000_000, 0, &system_program::id()))
                    .mode(AccountMode::Delegated)
                    .build(),
            );

            map.insert(
                MAGIC_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
            );

            map.insert(
                fee_vault_pubkey,
                AccountBuilder::from(AccountSharedData::new(0, 0, &system_program::id()))
                    .mode(AccountMode::Delegated)
                    .build(),
            );

            for committee in committees {
                map.insert(
                    *committee,
                    AccountBuilder::from(AccountSharedData::new(0, 0, &program))
                        .mode(AccountMode::Delegated)
                        .build(),
                );
            }

            map
        };

        ensure_started_validator(&mut account_data, Some(nonces));

        let transaction_accounts = vec![(
            clock::id(),
            create_account_shared_data_for_test(&get_clock()),
        )];

        (account_data, transaction_accounts)
    }

    #[test]
    #[serial]
    fn scheduling_captures_post_fee_balances() {
        use magic_program_api::args::{
            CommitAndUndelegateArgs, CommitTypeArgs, MagicBaseIntentArgs, UndelegateTypeArgs,
        };

        let payer = Keypair::new();
        let other = Pubkey::new_unique();
        for undelegate in [false, true] {
            let commit = CommitTypeArgs::Standalone(vec![3, 4]);
            let intents = if undelegate {
                let commit = CommitAndUndelegateArgs {
                    commit_type: commit,
                    undelegate_type: UndelegateTypeArgs::Standalone,
                };
                [
                    MagicBaseIntentArgs::CommitAndUndelegate(commit.clone()),
                    MagicBaseIntentArgs::CommitFinalizeAndUndelegate(commit),
                ]
            } else {
                [
                    MagicBaseIntentArgs::Commit(commit.clone()),
                    MagicBaseIntentArgs::CommitFinalize(commit),
                ]
            };
            let instructions = [
                if undelegate {
                    MagicBlockInstruction::ScheduleCommitAndUndelegate
                } else {
                    MagicBlockInstruction::ScheduleCommit
                },
                MagicBlockInstruction::ScheduleBaseIntent(intents[0].clone()),
                MagicBlockInstruction::ScheduleBaseIntent(intents[1].clone()),
                MagicBlockInstruction::ScheduleIntentBundle(intents[0].clone().into()),
                MagicBlockInstruction::ScheduleIntentBundle(intents[1].clone().into()),
            ];
            for instruction in instructions {
                for committee in [payer.pubkey(), other] {
                    for nonce in [ACTUAL_COMMIT_LIMIT - 1, ACTUAL_COMMIT_LIMIT] {
                        let fee =
                            if nonce == ACTUAL_COMMIT_LIMIT { COMMIT_FEE_LAMPORTS } else { 0 };
                        let (mut account_data, mut transaction_accounts) =
                            prepare_delegated_payer_transaction(
                                &payer,
                                system_program::id(),
                                &[committee],
                                StubNonces::PerAccount(HashMap::from([(committee, nonce)])),
                            );
                        account_data.get_mut(&payer.pubkey()).unwrap().set_lamports(3_000_000);
                        let fee_vault = magic_fee_vault_pubkey();
                        let mut ix = Instruction::new_with_wincode(
                            crate::id(),
                            &instruction,
                            vec![
                                AccountMeta::new(payer.pubkey(), true),
                                AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
                                AccountMeta::new(fee_vault, false),
                                AccountMeta::new(committee, true),
                                AccountMeta::new(fee_vault, true),
                            ],
                        );
                        if matches!(
                            instruction,
                            MagicBlockInstruction::ScheduleCommitAndUndelegate
                        ) {
                            // Standalone undelegation processes repeated accounts only once.
                            ix.accounts.push(AccountMeta::new(committee, true));
                        }
                        extend_transaction_accounts_from_ix(
                            &ix,
                            &mut account_data,
                            &mut transaction_accounts,
                        );
                        let keys: Vec<_> =
                            transaction_accounts.iter().map(|(key, _)| *key).collect();
                        let accounts = process_instruction(
                            &ix.data,
                            transaction_accounts,
                            ix.accounts,
                            Ok(()),
                        );
                        let account = |key| &accounts[keys.iter().position(|k| *k == key).unwrap()];
                        assert_eq!(account(payer.pubkey()).lamports(), 3_000_000 - fee);
                        assert_eq!(account(fee_vault).lamports(), fee);
                        let context =
                            MagicContext::deserialize(account(MAGIC_CONTEXT_PUBKEY).data())
                                .unwrap();
                        let committed =
                            context.scheduled_base_intents[0].get_all_committed_accounts();
                        assert_eq!(
                            committed.iter().map(|snapshot| snapshot.pubkey).collect::<Vec<_>>(),
                            [committee, fee_vault]
                        );
                        for snapshot in &committed {
                            assert_eq!(
                                snapshot.account.lamports,
                                account(snapshot.pubkey).lamports()
                            );
                            assert_eq!(snapshot.account.owner, system_program::id());
                            let expected_id =
                                if snapshot.pubkey == committee { nonce + 1 } else { 1 };
                            assert_eq!(snapshot.commit_id, expected_id);
                            assert_eq!(
                                u64::from(account(snapshot.pubkey).commit_seq()),
                                expected_id
                            );
                            assert!(account(snapshot.pubkey).is(if undelegate {
                                AccountMode::Undelegating
                            } else {
                                AccountMode::Delegated
                            }));
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[serial]
    fn test_schedule_commit_delegated_payer_only_charges_above_limit() {
        let payer = Keypair::from_seed(b"delegated_payer_only_above_limit_").unwrap();
        let program = Pubkey::new_unique();
        let committee_above = Pubkey::new_unique(); // nonce == limit → charged
        let committee_at = Pubkey::new_unique(); // nonce < limit → free

        let mut per_account = HashMap::new();
        per_account.insert(committee_above, ACTUAL_COMMIT_LIMIT);
        per_account.insert(committee_at, ACTUAL_COMMIT_LIMIT - 1);

        let (mut account_data, mut transaction_accounts) = prepare_delegated_payer_transaction(
            &payer,
            program,
            &[committee_above, committee_at],
            StubNonces::PerAccount(per_account),
        );

        let ix = InstructionUtils::schedule_commit_with_delegated_payer_instruction(
            &payer.pubkey(),
            vec![committee_above, committee_at],
        );
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        let accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        // Only committee_above is charged → vault receives exactly one fee
        let vault = accounts
            .iter()
            .find(|a| a.lamports() == COMMIT_FEE_LAMPORTS)
            .expect("fee vault should have COMMIT_FEE_LAMPORTS");
        assert_eq!(vault.lamports(), COMMIT_FEE_LAMPORTS);

        // Payer debited by exactly one fee
        assert!(accounts.iter().any(
            |a| a.lamports() == 1_000_000 - COMMIT_FEE_LAMPORTS && a.is(AccountMode::Delegated)
        ));
    }

    #[test]
    #[serial]
    /// An optional vault is skipped as a committee and is not charged.
    fn test_schedule_commit_optional_fee_vault_not_required() {
        let payer = Keypair::from_seed(b"schedule_commit_optional_vault__").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        let (mut account_data, mut transaction_accounts, fee_vault) =
            prepare_schedule_accounts(&payer, false, false, Some(true));
        let committee_acc = AccountBuilder::from(AccountSharedData::new(0, 0, &program))
            .mode(AccountMode::Delegated)
            .build();
        account_data.insert(committee, committee_acc);

        let ix = instruction_from_account_metas(vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            AccountMeta::new(fee_vault.unwrap(), false),
            AccountMeta::new(committee, true),
        ]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        let accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        accounts
            .iter()
            .find(|account| {
                account.is(AccountMode::Delegated)
                    && account.lamports() == 0
                    && account.owner() == &system_program::id()
            })
            .expect("fee vault should be validated but not charged");
    }

    #[test]
    #[serial]
    fn test_schedule_commit_delegated_payer_without_vault_errors() {
        let payer = Keypair::from_seed(b"delegated_payer_no_vault_________").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // Build account map with a delegated payer but NO fee vault entry
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(
                payer.pubkey(),
                AccountBuilder::from(AccountSharedData::new(1_000_000, 0, &system_program::id()))
                    .mode(AccountMode::Delegated)
                    .build(),
            );
            map.insert(
                MAGIC_CONTEXT_PUBKEY,
                AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
            );
            map.insert(
                committee,
                AccountBuilder::from(AccountSharedData::new(0, 0, &program))
                    .mode(AccountMode::Delegated)
                    .build(),
            );
            map
        };
        ensure_started_validator(&mut account_data, Some(StubNonces::Global(0)));

        let mut transaction_accounts = vec![(
            clock::id(),
            create_account_shared_data_for_test(&get_clock()),
        )];

        // Use the plain schedule_commit_instruction — no vault account included
        let ix = InstructionUtils::schedule_commit_instruction(&payer.pubkey(), vec![committee]);
        extend_transaction_accounts_from_ix(&ix, &mut account_data, &mut transaction_accounts);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingAccount),
        );
    }
}

/// Exercise reservations in real Engine transactions, including a failure after
/// scheduling. The instruction harness alone cannot prove transaction rollback.
#[tokio::test]
#[serial_test::serial]
async fn commit_reservations_are_atomic_and_reused_after_rollback() {
    use crate::{magicblock_processor::Entrypoint, utils::instruction_utils::InstructionUtils};
    use engine::testkit::{Pacing, TestEngine};
    use keeper::testkit::{Dirs, keeper_builder};
    use magic_program_api::args::{CommitAndUndelegateArgs, CommitTypeArgs, UndelegateTypeArgs};
    use solana_program_runtime::solana_sbpf::program::BuiltinFunctionDefinition;
    use solana_transaction::Transaction;

    let dirs = Dirs::default();
    let mut builder = keeper_builder(&dirs);
    builder.builtins.insert(crate::id(), (Entrypoint::vm, Entrypoint::codegen));
    builder.possibly_missing_accounts.insert(
        MAGIC_CONTEXT_PUBKEY,
        AccountBuilder::from(magic_context_account()).mode(AccountMode::Magic).build(),
    );
    let committee = Pubkey::new_unique();
    builder.possibly_missing_accounts.insert(
        committee,
        AccountBuilder::from(AccountSharedData::new(1_000_000, 0, &system_program::id()))
            .mode(AccountMode::Delegated)
            .commit_seq(3)
            .build(),
    );
    let overflow = Pubkey::new_unique();
    builder.possibly_missing_accounts.insert(
        overflow,
        AccountBuilder::from(AccountSharedData::new(1_000_000, 0, &system_program::id()))
            .mode(AccountMode::Delegated)
            .commit_seq(u32::MAX)
            .build(),
    );
    let payer = Keypair::new();
    let poor_payer = Keypair::new();
    let fee_vault = Pubkey::find_program_address(
        &[b"magic-fee-vault", builder.authority.pubkey().as_ref()],
        &DELEGATION_PROGRAM_ID,
    )
    .0;
    for (key, lamports, commit_seq) in [
        (payer.pubkey(), 3_000_000, ACTUAL_COMMIT_LIMIT as u32),
        (poor_payer.pubkey(), 900_000, ACTUAL_COMMIT_LIMIT as u32),
        (fee_vault, 1_000_000, 0),
    ] {
        builder.possibly_missing_accounts.insert(
            key,
            AccountBuilder::from(AccountSharedData::new(lamports, 0, &system_program::id()))
                .mode(AccountMode::Delegated)
                .commit_seq(commit_seq)
                .build(),
        );
    }
    let engine = TestEngine::from_builder(dirs, builder, Pacing::External).await;
    let mut schedule = InstructionUtils::schedule_commit_instruction(
        &engine.authority(),
        vec![committee, committee],
    );
    for meta in &mut schedule.accounts[2..] {
        meta.is_signer = false;
    }
    let bundle = Instruction::new_with_wincode(
        crate::id(),
        &MagicBlockInstruction::ScheduleIntentBundle(MagicIntentBundleArgs {
            commit_finalize: Some(CommitTypeArgs::Standalone(vec![2])),
            ..Default::default()
        }),
        schedule.accounts[..3].to_vec(),
    );
    let fail = Instruction {
        program_id: crate::id(),
        accounts: vec![],
        data: vec![255],
    };
    let initial_context = engine.get_account(MAGIC_CONTEXT_PUBKEY).unwrap().data().to_vec();
    for payer in [&payer, &poor_payer] {
        let initial_payer = engine.get_account(payer.pubkey()).unwrap();
        for undelegate in [false, true] {
            let commit = CommitTypeArgs::Standalone(vec![0]);
            let args = if undelegate {
                MagicIntentBundleArgs {
                    commit_finalize_and_undelegate: Some(CommitAndUndelegateArgs {
                        commit_type: commit,
                        undelegate_type: UndelegateTypeArgs::Standalone,
                    }),
                    ..Default::default()
                }
            } else {
                MagicIntentBundleArgs {
                    commit_finalize: Some(commit),
                    ..Default::default()
                }
            };
            // Duplicate accounts are rejected during construction, after charging
            // the fee. The transaction must roll back both fees and reserved IDs.
            let mut duplicate_args = args.clone();
            let duplicate_commit = if undelegate {
                &mut duplicate_args.commit_finalize_and_undelegate.as_mut().unwrap().commit_type
            } else {
                duplicate_args.commit_finalize.as_mut().unwrap()
            };
            *duplicate_commit = CommitTypeArgs::Standalone(vec![0, 0]);
            for (instruction, duplicate) in [
                (
                    if undelegate {
                        MagicBlockInstruction::ScheduleCommitAndUndelegate
                    } else {
                        MagicBlockInstruction::ScheduleCommit
                    },
                    false,
                ),
                (MagicBlockInstruction::ScheduleIntentBundle(args), false),
                (
                    MagicBlockInstruction::ScheduleIntentBundle(duplicate_args),
                    true,
                ),
            ] {
                let ix = Instruction::new_with_wincode(
                    crate::id(),
                    &instruction,
                    vec![
                        AccountMeta::new(payer.pubkey(), true),
                        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
                        AccountMeta::new(fee_vault, false),
                        AccountMeta::new(payer.pubkey(), true),
                    ],
                );
                let transaction = Transaction::new_signed_with_payer(
                    &[ix, fail.clone()],
                    Some(&engine.authority()),
                    &[engine.signer(), payer],
                    engine.blockhash(),
                );
                let result = engine.execute(transaction).await;
                if payer.pubkey() == poor_payer.pubkey() {
                    assert_matches!(
                        result,
                        Err(solana_transaction::TransactionError::InstructionError(
                            0,
                            InstructionError::InsufficientFunds
                        ))
                    );
                } else if duplicate {
                    assert_matches!(
                        result,
                        Err(solana_transaction::TransactionError::InstructionError(
                            0,
                            InstructionError::InvalidInstructionData
                        ))
                    );
                } else {
                    assert_matches!(
                        result,
                        Err(solana_transaction::TransactionError::InstructionError(1, _))
                    );
                }
                let account = engine.get_account(payer.pubkey()).unwrap();
                assert_eq!(account.lamports(), initial_payer.lamports());
                assert_eq!(account.commit_seq(), initial_payer.commit_seq());
                assert_eq!(account.owner(), initial_payer.owner());
                assert!(account.is(AccountMode::Delegated));
                assert_eq!(engine.get_account(fee_vault).unwrap().lamports(), 1_000_000);
                assert_eq!(
                    engine.get_account(MAGIC_CONTEXT_PUBKEY).unwrap().data(),
                    initial_context
                );
            }
        }
    }
    assert!(engine.execute(&[schedule.clone(), bundle.clone(), fail]).await.is_err());
    assert_eq!(engine.get_account(committee).unwrap().commit_seq(), 3);
    assert_eq!(
        engine.get_account(MAGIC_CONTEXT_PUBKEY).unwrap().data(),
        initial_context
    );

    let mut readonly = bundle.clone();
    readonly.accounts[2].is_writable = false;
    assert!(matches!(
        engine.execute(&[readonly]).await,
        Err(solana_transaction::TransactionError::InstructionError(
            _,
            InstructionError::ReadonlyDataModified
        ))
    ));
    let mut overflowing = bundle.clone();
    overflowing.accounts[2].pubkey = overflow;
    assert!(matches!(
        engine.execute(&[overflowing]).await,
        Err(solana_transaction::TransactionError::InstructionError(
            _,
            InstructionError::ArithmeticOverflow
        ))
    ));
    assert_eq!(engine.get_account(overflow).unwrap().commit_seq(), u32::MAX);
    assert_eq!(
        engine.get_account(MAGIC_CONTEXT_PUBKEY).unwrap().data(),
        initial_context
    );

    let instructions = [schedule, bundle];
    let first = engine.simulate(&instructions).await.unwrap().result.unwrap();
    let replay = engine.simulate(&instructions).await.unwrap().result.unwrap();
    assert_eq!(first.execution_details.status, Ok(()));
    assert_eq!(
        first.execution_details.log_messages,
        replay.execution_details.log_messages
    );
    let scheduled_context = |accounts: &[(Pubkey, AccountSharedData)]| {
        accounts
            .iter()
            .find(|(key, _)| key == &MAGIC_CONTEXT_PUBKEY)
            .unwrap()
            .1
            .data()
            .to_vec()
    };
    assert_eq!(
        scheduled_context(&first.loaded_transaction.accounts),
        scheduled_context(&replay.loaded_transaction.accounts)
    );
    engine.execute(&instructions).await.unwrap();
    assert_eq!(engine.get_account(committee).unwrap().commit_seq(), 5);
    let context =
        MagicContext::deserialize(engine.get_account(MAGIC_CONTEXT_PUBKEY).unwrap().data())
            .unwrap();
    let ids: Vec<_> = context
        .scheduled_base_intents
        .iter()
        .flat_map(|intent| intent.get_all_committed_accounts())
        .map(|account| account.commit_id)
        .collect();
    assert_eq!(ids, vec![4, 5]);
    let logs = first.execution_details.log_messages.unwrap();
    for intent in &context.scheduled_base_intents {
        assert!(logs.contains(&format!(
            "ScheduledCommitSent outbox: {}",
            mbv_core::intent::outbox::outbox_intent_pda(intent.intent_id)
        )));
    }
    assert!(!logs.iter().any(|line| line.starts_with("ScheduledCommitSent signature:")));
    engine.close().await;
}
