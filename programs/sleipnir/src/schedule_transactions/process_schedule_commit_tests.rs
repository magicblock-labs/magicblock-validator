use std::collections::HashMap;

use assert_matches::assert_matches;
use sleipnir_core::magic_program::MAGIC_CONTEXT_PUBKEY;
use solana_sdk::{
    account::{
        create_account_shared_data_for_test, AccountSharedData, ReadableAccount,
    },
    clock,
    fee_calculator::DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE,
    instruction::{AccountMeta, Instruction, InstructionError},
    pubkey::Pubkey,
    signature::Keypair,
    signer::{SeedDerivable, Signer},
    system_program,
    sysvar::SysvarId,
};
use test_tools_core::init_logger;

use crate::{
    magic_context::MagicContext,
    schedule_transactions::transaction_scheduler::TransactionScheduler,
    sleipnir_instruction::{
        accept_scheduled_commits_instruction,
        schedule_commit_and_undelegate_instruction,
        schedule_commit_instruction, SleipnirInstruction,
    },
    test_utils::{ensure_funded_validator_authority, process_instruction},
    utils::DELEGATION_PROGRAM_ID,
    ScheduledCommit,
};

// For the scheduling itself and the debit to fund the scheduled transaction
const REQUIRED_TX_COST: u64 = DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE * 2;

fn get_clock() -> clock::Clock {
    clock::Clock {
        slot: 100,
        unix_timestamp: 1_000,
        epoch_start_timestamp: 0,
        epoch: 10,
        leader_schedule_epoch: 10,
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
        map.insert(committee, AccountSharedData::new(0, 0, &program));
        map
    };
    ensure_funded_validator_authority(&mut account_data);

    let transaction_accounts: Vec<(Pubkey, AccountSharedData)> = vec![(
        clock::Clock::id(),
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
) -> PreparedTransactionThreeCommittees {
    let program = Pubkey::new_unique();
    let (committee_uno, committee_dos, committee_tres) =
        committees.unwrap_or((
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
        map.insert(committee_uno, AccountSharedData::new(0, 0, &program));
        map.insert(committee_dos, AccountSharedData::new(0, 0, &program));
        map.insert(committee_tres, AccountSharedData::new(0, 0, &program));
        map
    };
    ensure_funded_validator_authority(&mut accounts_data);

    let transaction_accounts: Vec<(Pubkey, AccountSharedData)> = vec![(
        clock::Clock::id(),
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

fn find_magic_context_account(
    accounts: &[AccountSharedData],
) -> Option<&AccountSharedData> {
    accounts
        .iter()
        .find(|acc| acc.owner() == &crate::id() && acc.lamports() == u64::MAX)
}

fn assert_non_accepted_commits<'a>(
    processed_scheduled: &'a [AccountSharedData],
    payer: &Pubkey,
    expected_non_accepted_commits: usize,
) -> &'a AccountSharedData {
    let magic_context_acc = find_magic_context_account(processed_scheduled)
        .expect("magic context account not found");
    let magic_context =
        bincode::deserialize::<MagicContext>(magic_context_acc.data()).unwrap();

    let accepted_scheduled_commits =
        TransactionScheduler::default().get_scheduled_commits_by_payer(payer);
    assert_eq!(
        magic_context.scheduled_commits.len(),
        expected_non_accepted_commits
    );
    assert_eq!(accepted_scheduled_commits.len(), 0);

    magic_context_acc
}

fn assert_accepted_commits(
    processed_accepted: &[AccountSharedData],
    payer: &Pubkey,
    expected_scheduled_commits: usize,
) -> Vec<ScheduledCommit> {
    let magic_context_acc = find_magic_context_account(processed_accepted)
        .expect("magic context account not found");
    let magic_context =
        bincode::deserialize::<MagicContext>(magic_context_acc.data()).unwrap();

    let scheduled_commits =
        TransactionScheduler::default().get_scheduled_commits_by_payer(payer);

    assert_eq!(magic_context.scheduled_commits.len(), 0);
    assert_eq!(scheduled_commits.len(), expected_scheduled_commits);

    scheduled_commits
}

fn extend_transaction_accounts_from_ix(
    ix: &Instruction,
    account_data: &mut HashMap<Pubkey, AccountSharedData>,
    transaction_accounts: &mut Vec<(Pubkey, AccountSharedData)>,
) {
    transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
        account_data
            .remove(&acc.pubkey)
            .map(|shared_data| (acc.pubkey, shared_data))
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
    scheduled_commits: &[ScheduledCommit],
    payer: &Pubkey,
    owner: &Pubkey,
    committees: &[Pubkey],
    expected_request_undelegation: bool,
) {
    let commit = &scheduled_commits[0];
    let test_clock = get_clock();
    assert_matches!(
        commit,
        ScheduledCommit {
            id,
            slot,
            accounts,
            payer: p,
            owner: o,
            blockhash: _,
            commit_sent_transaction,
            request_undelegation,
        } => {
            assert!(id >= &0);
            assert_eq!(slot, &test_clock.slot);
            assert_eq!(p, payer);
            assert_eq!(o, owner);
            assert_eq!(accounts, committees);
            let instruction = SleipnirInstruction::ScheduledCommitSent(*id);
            assert_eq!(commit_sent_transaction.data(0), instruction.try_to_vec().unwrap());
            assert_eq!(*request_undelegation, expected_request_undelegation);
        }
    );
}

#[test]
fn test_schedule_commit_single_account_success() {
    init_logger!();
    let payer =
        Keypair::from_seed(b"schedule_commit_single_account_success").unwrap();
    let program = Pubkey::new_unique();
    let committee = Pubkey::new_unique();

    // 1. We run the transaction that registers the intent to schedule a commit
    let (processed_scheduled, magic_context_acc) = {
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer, program, committee,
            );

        let ix = schedule_commit_instruction(&payer.pubkey(), vec![committee]);

        extend_transaction_accounts_from_ix(
            &ix,
            &mut account_data,
            &mut transaction_accounts,
        );

        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts.clone(),
            ix.accounts,
            Ok(()),
        );

        // At this point the intent to commit was added to the magic context account,
        // but not yet accepted
        let magic_context_acc = assert_non_accepted_commits(
            &processed_scheduled,
            &payer.pubkey(),
            1,
        );

        (processed_scheduled.clone(), magic_context_acc.clone())
    };

    // 2. We run the transaction that accepts the scheduled commit
    {
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer, program, committee,
            );

        let ix = accept_scheduled_commits_instruction();
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
            assert_accepted_commits(&processed_accepted, &payer.pubkey(), 1);

        assert_first_commit(
            &scheduled_commits,
            &payer.pubkey(),
            &program,
            &[committee],
            false,
        );
    }
    let committed_account = processed_scheduled.last().unwrap();
    assert_eq!(*committed_account.owner(), program);
}

#[test]
fn test_schedule_commit_single_account_and_request_undelegate_success() {
    init_logger!();
    let payer =
        Keypair::from_seed(b"single_account_with_undelegate_success").unwrap();
    let program = Pubkey::new_unique();
    let committee = Pubkey::new_unique();

    // 1. We run the transaction that registers the intent to schedule a commit
    let (processed_scheduled, magic_context_acc) = {
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer, program, committee,
            );

        let ix = schedule_commit_and_undelegate_instruction(
            &payer.pubkey(),
            vec![committee],
        );

        extend_transaction_accounts_from_ix(
            &ix,
            &mut account_data,
            &mut transaction_accounts,
        );

        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts.clone(),
            ix.accounts,
            Ok(()),
        );

        // At this point the intent to commit was added to the magic context account,
        // but not yet accepted
        let magic_context_acc = assert_non_accepted_commits(
            &processed_scheduled,
            &payer.pubkey(),
            1,
        );

        (processed_scheduled.clone(), magic_context_acc.clone())
    };

    // 2. We run the transaction that accepts the scheduled commit
    {
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer, program, committee,
            );

        let ix = accept_scheduled_commits_instruction();
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
            assert_accepted_commits(&processed_accepted, &payer.pubkey(), 1);

        assert_first_commit(
            &scheduled_commits,
            &payer.pubkey(),
            &program,
            &[committee],
            true,
        );
    }
    let committed_account = processed_scheduled.last().unwrap();
    assert_eq!(*committed_account.owner(), DELEGATION_PROGRAM_ID);
}

#[test]
fn test_schedule_commit_three_accounts_success() {
    init_logger!();

    let payer =
        Keypair::from_seed(b"schedule_commit_three_accounts_success").unwrap();

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
        } = prepare_transaction_with_three_committees(&payer, None);

        let ix = schedule_commit_instruction(
            &payer.pubkey(),
            vec![committee_uno, committee_dos, committee_tres],
        );
        extend_transaction_accounts_from_ix(
            &ix,
            &mut accounts_data,
            &mut transaction_accounts,
        );

        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        // At this point the intent to commit was added to the magic context account,
        // but not yet accepted
        let magic_context_acc = assert_non_accepted_commits(
            &processed_scheduled,
            &payer.pubkey(),
            1,
        );

        (
            processed_scheduled.clone(),
            magic_context_acc.clone(),
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
        );

        let ix = accept_scheduled_commits_instruction();
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
            assert_accepted_commits(&processed_accepted, &payer.pubkey(), 1);

        assert_first_commit(
            &scheduled_commits,
            &payer.pubkey(),
            &program,
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
fn test_schedule_commit_three_accounts_and_request_undelegate_success() {
    let payer =
        Keypair::from_seed(b"three_accounts_and_request_undelegate_success")
            .unwrap();

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
        } = prepare_transaction_with_three_committees(&payer, None);

        let ix = schedule_commit_and_undelegate_instruction(
            &payer.pubkey(),
            vec![committee_uno, committee_dos, committee_tres],
        );

        extend_transaction_accounts_from_ix(
            &ix,
            &mut accounts_data,
            &mut transaction_accounts,
        );

        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        // At this point the intent to commit was added to the magic context account,
        // but not yet accepted
        let magic_context_acc = assert_non_accepted_commits(
            &processed_scheduled,
            &payer.pubkey(),
            1,
        );

        (
            processed_scheduled.clone(),
            magic_context_acc.clone(),
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
        );

        let ix = accept_scheduled_commits_instruction();
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
            assert_accepted_commits(&processed_accepted, &payer.pubkey(), 1);

        assert_first_commit(
            &scheduled_commits,
            &payer.pubkey(),
            &program,
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
    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
    ];
    for pubkey in &pdas {
        account_metas.push(AccountMeta::new_readonly(*pubkey, true));
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

fn instruction_from_account_metas(
    account_metas: Vec<AccountMeta>,
) -> solana_sdk::instruction::Instruction {
    Instruction::new_with_bincode(
        crate::id(),
        &SleipnirInstruction::ScheduleCommit,
        account_metas,
    )
}

#[test]
fn test_schedule_commit_no_pdas_provided_to_ix() {
    init_logger!();

    let payer =
        Keypair::from_seed(b"schedule_commit_no_pdas_provided_to_ix").unwrap();

    let PreparedTransactionThreeCommittees {
        mut accounts_data,
        mut transaction_accounts,
        ..
    } = prepare_transaction_with_three_committees(&payer, None);

    let ix = instruction_from_account_metas(
        get_account_metas_for_schedule_commit(&payer.pubkey(), vec![]),
    );
    extend_transaction_accounts_from_ix(
        &ix,
        &mut accounts_data,
        &mut transaction_accounts,
    );

    process_instruction(
        ix.data.as_slice(),
        transaction_accounts,
        ix.accounts,
        Err(InstructionError::NotEnoughAccountKeys),
    );
}

#[test]
fn test_schedule_commit_three_accounts_second_not_owned_by_program() {
    init_logger!();

    let payer = Keypair::from_seed(b"three_accounts_last_not_owned_by_program")
        .unwrap();

    let PreparedTransactionThreeCommittees {
        mut accounts_data,
        committee_uno,
        committee_dos,
        committee_tres,
        mut transaction_accounts,
        ..
    } = prepare_transaction_with_three_committees(&payer, None);

    accounts_data.insert(
        committee_dos,
        AccountSharedData::new(0, 0, &Pubkey::new_unique()),
    );

    let ix = instruction_from_account_metas(
        account_metas_last_committee_not_signer(
            &payer.pubkey(),
            vec![committee_uno, committee_dos, committee_tres],
        ),
    );
    extend_transaction_accounts_from_ix(
        &ix,
        &mut accounts_data,
        &mut transaction_accounts,
    );

    process_instruction(
        ix.data.as_slice(),
        transaction_accounts,
        ix.accounts,
        Err(InstructionError::InvalidAccountOwner),
    );
}
