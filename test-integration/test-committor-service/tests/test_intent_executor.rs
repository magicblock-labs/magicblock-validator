use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread::sleep,
    time::Duration,
};

use borsh::to_vec;
use dlp::pda::ephemeral_balance_pda_from_payer;
use futures::future::join_all;
use magicblock_committor_program::pdas;
use magicblock_committor_service::{
    intent_executor::{
        error::TransactionStrategyExecutionError,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        ExecutionOutput, IntentExecutor, IntentExecutorImpl,
    },
    persist::IntentPersisterImpl,
    tasks::{
        task_builder::{TaskBuilderImpl, TasksBuilder},
        task_strategist::{TaskStrategist, TransactionStrategy},
    },
    transaction_preparator::TransactionPreparatorImpl,
};
use magicblock_program::{
    args::ShortAccountMeta,
    magic_scheduled_base_intent::{
        BaseAction, CommitAndUndelegate, CommitType, CommittedAccount,
        MagicBaseIntent, ProgramArgs, ScheduledBaseIntent, UndelegateType,
    },
    validator::validator_authority_id,
};
use magicblock_table_mania::TableMania;
use program_flexi_counter::{
    instruction::FlexiCounterInstruction, state::FlexiCounter,
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use crate::{
    common::TestFixture,
    utils::{
        ensure_validator_authority,
        transactions::{
            fund_validator_auth_and_ensure_validator_fees_vault,
            init_and_delegate_account_on_chain,
        },
    },
};

mod common;
mod utils;

const ACTOR_ESCROW_INDEX: u8 = 1;

struct TestEnv {
    fixture: TestFixture,
    task_info_fetcher: Arc<CacheTaskInfoFetcher>,
    intent_executor:
        IntentExecutorImpl<TransactionPreparatorImpl, CacheTaskInfoFetcher>,
    pre_test_tablemania_state: HashMap<Pubkey, usize>,
}

impl TestEnv {
    async fn setup() -> Self {
        let validator_auth = ensure_validator_authority();
        let fixture = TestFixture::new_with_keypair(validator_auth).await;
        fund_validator_auth_and_ensure_validator_fees_vault(&fixture.authority)
            .await;

        let transaction_preparator = fixture.create_transaction_preparator();
        let task_info_fetcher =
            Arc::new(CacheTaskInfoFetcher::new(fixture.rpc_client.clone()));

        let tm = &fixture.table_mania;
        let mut pre_test_tablemania_state = HashMap::new();
        for pubkey in tm.active_table_pubkeys().await {
            let count = tm.get_pubkey_refcount(&pubkey).await.unwrap_or(0);
            pre_test_tablemania_state.insert(pubkey, count);
        }

        let intent_executor = IntentExecutorImpl::new(
            fixture.rpc_client.clone(),
            transaction_preparator,
            task_info_fetcher.clone(),
        );

        Self {
            fixture,
            task_info_fetcher,
            intent_executor,
            pre_test_tablemania_state,
        }
    }
}

#[tokio::test]
async fn test_commit_id_error_parsing() {
    const COUNTER_SIZE: u64 = 70;
    const EXPECTED_ERR_MSG: &str = "Accounts committed with an invalid Commit id: Error processing Instruction 2: custom program error: 0xc";

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;
    let (counter_auth, account) = setup_counter(COUNTER_SIZE).await;
    let intent = create_intent(
        vec![CommittedAccount {
            pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
            account,
        }],
        true,
    );

    // Invalidate ids before execution
    task_info_fetcher
        .fetch_next_commit_ids(&intent.get_committed_pubkeys().unwrap())
        .await
        .unwrap();

    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &intent,
    )
    .await;
    let execution_result = intent_executor
        .prepare_and_execute_strategy(
            &mut transaction_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(execution_result.is_ok(), "Preparation is expected to pass!");

    // Verify that we got CommitIdError
    let execution_result = execution_result.unwrap();
    assert!(execution_result.is_err());
    let err = execution_result.unwrap_err();
    assert!(matches!(
        err,
        TransactionStrategyExecutionError::CommitIDError(_)
    ));
    assert_eq!(err.to_string(), EXPECTED_ERR_MSG.to_string(),);
}

#[tokio::test]
async fn test_action_error_parsing() {
    const COUNTER_SIZE: u64 = 70;
    const EXPECTED_ERR_MSG: &str = "User supplied actions are ill-formed: Error processing Instruction 5: Program arithmetic overflowed";

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    let (counter_auth, account) = setup_counter(COUNTER_SIZE).await;
    setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
        .await;

    let committed_account = CommittedAccount {
        pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
        account,
    };

    // Create Intent with invalid action
    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action = failing_undelegate_action(
        counter_auth.pubkey(),
        committed_account.pubkey,
    );
    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    let scheduled_intent = create_scheduled_intent(base_intent);
    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &scheduled_intent,
    )
    .await;
    let execution_result = intent_executor
        .prepare_and_execute_strategy(
            &mut transaction_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(execution_result.is_ok(), "Preparation is expected to pass!");

    // Verify that we got ActionsError
    let execution_result = execution_result.unwrap();
    assert!(execution_result.is_err());
    let execution_err = execution_result.unwrap_err();
    assert!(matches!(
        execution_err,
        TransactionStrategyExecutionError::ActionsError(_)
    ));
    assert_eq!(execution_err.to_string(), EXPECTED_ERR_MSG.to_string());
}

#[tokio::test]
async fn test_cpi_limits_error_parsing() {
    const COUNTER_SIZE: u64 = 102;
    const COUNTER_NUM: u64 = 10;
    const EXPECTED_ERR_MSG: &str = "Max instruction trace length exceeded: Error processing Instruction 26: Max instruction trace length exceeded";

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    let counters = (0..COUNTER_NUM).map(|_| async {
        let (counter_auth, account) = setup_counter(COUNTER_SIZE).await;
        setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
            .await;

        (counter_auth, account)
    });

    let counters = join_all(counters).await;
    let committed_accounts: Vec<_> = counters
        .iter()
        .map(|(counter, account)| CommittedAccount {
            pubkey: FlexiCounter::pda(&counter.pubkey()).0,
            account: account.clone(),
        })
        .collect();

    let scheduled_intent = create_intent(committed_accounts.clone(), true);
    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &scheduled_intent,
    )
    .await;
    let execution_result = intent_executor
        .prepare_and_execute_strategy(
            &mut transaction_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(execution_result.is_ok(), "Preparation is expected to pass!");

    let execution_result = execution_result.unwrap();
    assert!(
        execution_result.is_err(),
        "Execution of intent expected to fail"
    );
    let execution_err = execution_result.unwrap_err();
    assert!(matches!(
        execution_err,
        TransactionStrategyExecutionError::CpiLimitError(_)
    ));
    assert_eq!(execution_err.to_string(), EXPECTED_ERR_MSG.to_string());
}

#[tokio::test]
async fn test_commit_id_error_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let counter_auth = Keypair::new();
    let (pubkey, mut account) =
        init_and_delegate_account_on_chain(&counter_auth, COUNTER_SIZE).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount { pubkey, account };
    let intent = create_intent(vec![committed_account.clone()], false);

    // Invalidate commit nonce cache
    let res = task_info_fetcher
        .fetch_next_commit_ids(&[committed_account.pubkey])
        .await;
    assert!(res.is_ok());
    assert!(res.unwrap().contains_key(&committed_account.pubkey));

    // Now execute intent
    let res = intent_executor
        .execute(intent, None::<IntentPersisterImpl>)
        .await;
    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));

    let commit_ids_by_pk: HashMap<_, _> = [&committed_account]
        .iter()
        .map(|el| {
            (
                el.pubkey,
                task_info_fetcher.peek_commit_id(&el.pubkey).unwrap(),
            )
        })
        .collect();
    verify(
        &fixture.table_mania,
        fixture.rpc_client.get_inner(),
        &commit_ids_by_pk,
        &pre_test_tablemania_state,
        &[committed_account],
    )
    .await;
}

#[tokio::test]
async fn test_action_error_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
    };

    // Create Intent with invalid action
    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action =
        failing_undelegate_action(payer.pubkey(), committed_account.pubkey);
    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    let scheduled_intent = create_scheduled_intent(base_intent);
    let res = intent_executor
        .execute(scheduled_intent, None::<IntentPersisterImpl>)
        .await;
    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));

    verify_committed_accounts_state(
        fixture.rpc_client.get_inner(),
        &[committed_account],
    )
    .await;
    verify_table_mania_released(
        &fixture.table_mania,
        &pre_test_tablemania_state,
    )
    .await;
}

#[tokio::test]
async fn test_commit_id_and_action_errors_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
    };

    // Invalidate commit nonce cache
    let res = task_info_fetcher
        .fetch_next_commit_ids(&[committed_account.pubkey])
        .await;
    assert!(res.is_ok());
    assert!(res.unwrap().contains_key(&committed_account.pubkey));

    // Create Intent with invalid action
    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action =
        failing_undelegate_action(payer.pubkey(), committed_account.pubkey);
    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    let scheduled_intent = create_scheduled_intent(base_intent);
    let res = intent_executor
        .execute(scheduled_intent, None::<IntentPersisterImpl>)
        .await;
    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));

    verify_committed_accounts_state(
        fixture.rpc_client.get_inner(),
        &[committed_account],
    )
    .await;
    verify_table_mania_released(
        &fixture.table_mania,
        &pre_test_tablemania_state,
    )
    .await;
}

#[tokio::test]
async fn test_cpi_limits_error_recovery() {
    const COUNTER_SIZE: u64 = 102;
    const COUNTER_NUM: u64 = 10;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let counters = (0..COUNTER_NUM).map(|_| async {
        let (counter_auth, account) = setup_counter(COUNTER_SIZE).await;
        setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
            .await;

        (counter_auth, account)
    });

    let counters = join_all(counters).await;
    let committed_accounts: Vec<_> = counters
        .iter()
        .enumerate()
        .map(|(i, (counter, account))| {
            let data = FlexiCounter {
                label: format!("{}", i),
                count: i as u64,
                updates: i as u64,
            };

            let mut account = account.clone();
            account.data = to_vec(&data).unwrap();
            CommittedAccount {
                pubkey: FlexiCounter::pda(&counter.pubkey()).0,
                account: account.clone(),
            }
        })
        .collect();

    let scheduled_intent = create_intent(committed_accounts.clone(), true);
    // Form strategy that will fail due to CPI Limit
    // Recovery is to split this into 2 transactions: Commit & Finalize
    let strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &scheduled_intent,
    )
    .await;
    let execution_result = intent_executor
        .single_stage_execution_flow(
            scheduled_intent,
            strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(execution_result.is_ok(), "Intent expected to recover");
    assert!(matches!(
        execution_result.unwrap(),
        ExecutionOutput::TwoStage {
            commit_signature: _,
            finalize_signature: _,
        }
    ));

    let commit_ids_by_pk: HashMap<_, _> = committed_accounts
        .iter()
        .map(|el| {
            (
                el.pubkey,
                task_info_fetcher.peek_commit_id(&el.pubkey).unwrap(),
            )
        })
        .collect();
    verify(
        &fixture.table_mania,
        fixture.rpc_client.get_inner(),
        &commit_ids_by_pk,
        &pre_test_tablemania_state,
        &committed_accounts,
    )
    .await;
}

#[tokio::test]
async fn test_commit_id_actions_cpi_limit_errors_recovery() {
    const COUNTER_SIZE: u64 = 102;
    // COUNTER_NUM = 10 or larger result in CpiLimitError even with 2 stage execution
    const COUNTER_NUM: u64 = 9;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    // Prepare multiple counters; each needs an escrow (payer) to be able to execute base actions.
    // We also craft unique on-chain data so we can verify post-commit state exactly.
    let counters = (0..COUNTER_NUM).map(|_| async {
        let (counter_auth, account) = setup_counter(COUNTER_SIZE).await;
        setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
            .await;
        (counter_auth, account)
    });
    let counters = join_all(counters).await;

    // Build committed accounts with distinct serialized data to verify later
    let committed_accounts: Vec<_> = counters
        .iter()
        .enumerate()
        .map(|(i, (counter, account))| {
            let data = FlexiCounter {
                label: format!("acct-{i}"),
                count: i as u64,
                updates: (i as u64) * 2,
            };
            let mut account = account.clone();
            account.data = to_vec(&data).unwrap();
            CommittedAccount {
                pubkey: FlexiCounter::pda(&counter.pubkey()).0,
                account,
            }
        })
        .collect();

    // We use CommitAndUndelegate so initial single-stage flow is guaranteed to be heavy.
    let payer = setup_payer(fixture.rpc_client.get_inner()).await;

    let commit_action = CommitType::Standalone(committed_accounts.clone());
    let undelegate_action =
        failing_undelegate_action(payer.pubkey(), committed_accounts[0].pubkey);
    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });
    let scheduled_intent = create_scheduled_intent(base_intent);

    // Force CommitIDError by invalidating the commit-nonce cache before running
    let pubkeys: Vec<_> = committed_accounts.iter().map(|c| c.pubkey).collect();
    let mut invalidated_keys = task_info_fetcher
        .fetch_next_commit_ids(&pubkeys)
        .await
        .unwrap();

    // Build a single-flow strategy (commit + finalize in one go),
    // then run the single-stage execution flow that can recover by splitting into two stages.
    let strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &scheduled_intent,
    )
    .await;

    let res = intent_executor
        .single_stage_execution_flow(
            scheduled_intent,
            strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;

    println!("{:?}", res);
    // We expect recovery to succeed by splitting into two stages (commit, then finalize)
    assert!(
        res.is_ok(),
        "Expected recovery from CommitID, Actions, and CpiLimit errors"
    );
    assert!(matches!(
        res.unwrap(),
        ExecutionOutput::TwoStage {
            commit_signature: _,
            finalize_signature: _,
        }
    ));

    let commit_ids_by_pk: HashMap<_, _> = committed_accounts
        .iter()
        .map(|el| {
            (
                el.pubkey,
                task_info_fetcher.peek_commit_id(&el.pubkey).unwrap(),
            )
        })
        .collect();
    verify(
        &fixture.table_mania,
        fixture.rpc_client.get_inner(),
        &commit_ids_by_pk,
        &pre_test_tablemania_state,
        &committed_accounts,
    )
    .await;

    // Verify that incorrectly created buffers are also cleaned up
    invalidated_keys.values_mut().for_each(|el| *el += 1);
    verify_buffers_cleaned_up(
        fixture.rpc_client.get_inner(),
        &committed_accounts,
        &invalidated_keys,
    )
    .await;
}

fn failing_undelegate_action(
    escrow_authority: Pubkey,
    undelegated_account: Pubkey,
) -> UndelegateType {
    const PRIZE: u64 = 1_000_000;
    const BREAKING_DIFF: i64 = -1000000; // Breaks action

    let undelegate_action_data =
        FlexiCounterInstruction::UndelegateActionHandler {
            counter_diff: BREAKING_DIFF,
            amount: PRIZE,
        };

    let transfer_destination = Pubkey::new_unique();
    let account_metas = vec![
        ShortAccountMeta {
            pubkey: undelegated_account,
            is_writable: true,
        },
        ShortAccountMeta {
            pubkey: transfer_destination,
            is_writable: true,
        },
        ShortAccountMeta {
            pubkey: solana_sdk::system_program::id(),
            is_writable: false,
        },
    ];

    UndelegateType::WithBaseActions(vec![BaseAction {
        compute_units: 100_000,
        destination_program: program_flexi_counter::id(),
        escrow_authority,
        data_per_program: ProgramArgs {
            escrow_index: ACTOR_ESCROW_INDEX,
            data: to_vec(&undelegate_action_data).unwrap(),
        },
        account_metas_per_program: account_metas,
    }])
}

async fn setup_payer(rpc_client: &Arc<RpcClient>) -> Keypair {
    let payer = Keypair::new();
    setup_payer_with_keypair(&payer, rpc_client).await;

    payer
}

async fn setup_payer_with_keypair(
    payer: &Keypair,
    rpc_client: &Arc<RpcClient>,
) {
    let sig = rpc_client
        .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL)
        .await
        .unwrap();
    rpc_client
        .confirm_transaction_with_commitment(
            &sig,
            CommitmentConfig::finalized(),
        )
        .await
        .unwrap();

    sleep(Duration::from_secs(1));
    // Create actor escrow
    let ix = dlp::instruction_builder::top_up_ephemeral_balance(
        payer.pubkey(),
        payer.pubkey(),
        Some(LAMPORTS_PER_SOL / 2),
        Some(ACTOR_ESCROW_INDEX),
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        rpc_client.get_latest_blockhash().await.unwrap(),
    );
    rpc_client.send_and_confirm_transaction(&tx).await.unwrap();

    // Confirm actor escrow
    let escrow_pda =
        ephemeral_balance_pda_from_payer(&payer.pubkey(), ACTOR_ESCROW_INDEX);
    let rent = Rent::default().minimum_balance(0);
    assert_eq!(
        rpc_client.get_account(&escrow_pda).await.unwrap().lamports,
        LAMPORTS_PER_SOL / 2 + rent
    );
}

async fn setup_counter(counter_bytes: u64) -> (Keypair, Account) {
    let counter_auth = Keypair::new();
    let (_, mut account) =
        init_and_delegate_account_on_chain(&counter_auth, counter_bytes).await;

    account.owner = program_flexi_counter::id();
    (counter_auth, account)
}

fn create_intent(
    committed_accounts: Vec<CommittedAccount>,
    is_undelegate: bool,
) -> ScheduledBaseIntent {
    let base_intent = if is_undelegate {
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(committed_accounts),
            undelegate_action: UndelegateType::Standalone,
        })
    } else {
        MagicBaseIntent::Commit(CommitType::Standalone(committed_accounts))
    };

    create_scheduled_intent(base_intent)
}

fn create_scheduled_intent(
    base_intent: MagicBaseIntent,
) -> ScheduledBaseIntent {
    static INTENT_ID: AtomicU64 = AtomicU64::new(0);

    ScheduledBaseIntent {
        id: INTENT_ID.fetch_add(1, Ordering::Relaxed),
        slot: 10,
        blockhash: Hash::new_unique(),
        action_sent_transaction: Transaction::default(),
        payer: Pubkey::new_unique(),
        base_intent,
    }
}

async fn single_flow_transaction_strategy(
    authority: &Pubkey,
    task_info_fetcher: &Arc<CacheTaskInfoFetcher>,
    intent: &ScheduledBaseIntent,
) -> TransactionStrategy {
    let mut tasks = TaskBuilderImpl::commit_tasks(
        task_info_fetcher,
        intent,
        &None::<IntentPersisterImpl>,
    )
    .await
    .unwrap();
    let finalize_tasks =
        TaskBuilderImpl::finalize_tasks(task_info_fetcher, intent)
            .await
            .unwrap();
    tasks.extend(finalize_tasks);

    TaskStrategist::build_strategy(
        tasks,
        authority,
        &None::<IntentPersisterImpl>,
    )
    .unwrap()
}

async fn verify_committed_accounts_state(
    rpc_client: &Arc<RpcClient>,
    expected_accounts: &[CommittedAccount],
) {
    let assert_futs = expected_accounts.iter().map(|committed_account| async {
        let account = rpc_client
            .get_account(&committed_account.pubkey)
            .await
            .unwrap();
        assert_eq!(account.data, committed_account.account.data);
    });

    join_all(assert_futs).await;
}

async fn verify_buffers_cleaned_up(
    rpc_client: &Arc<RpcClient>,
    commited_accounts: &[CommittedAccount],
    commit_ids_by_pk: &HashMap<Pubkey, u64>,
) {
    let validator_auth = validator_authority_id();
    for committed_account in commited_accounts {
        let Some(commit_id) = commit_ids_by_pk.get(&committed_account.pubkey)
        else {
            continue;
        };
        let (buffer_pda, _) = pdas::buffer_pda(
            &validator_auth,
            &committed_account.pubkey,
            commit_id.to_le_bytes().as_slice(),
        );
        let buffer_account = rpc_client
            .get_account_with_commitment(&buffer_pda, rpc_client.commitment())
            .await
            .unwrap();

        assert_eq!(
            buffer_account.value, None,
            "Account expected to be closed after commit"
        );
    }
}

async fn verify_table_mania_released(
    table_mania: &TableMania,
    pre_start_state: &HashMap<Pubkey, usize>,
) {
    let mut pre_start_pubkeys: Vec<_> =
        pre_start_state.keys().copied().collect();
    pre_start_pubkeys.sort();

    let mut current_pubkeys = table_mania.active_table_pubkeys().await;
    current_pubkeys.sort();
    assert_eq!(pre_start_pubkeys, current_pubkeys);

    for (address, expected_count) in pre_start_state {
        assert_eq!(
            table_mania.get_pubkey_refcount(address).await,
            Some(*expected_count)
        )
    }
}

async fn verify(
    table_mania: &TableMania,
    rpc_client: &Arc<RpcClient>,
    commit_ids_by_pk: &HashMap<Pubkey, u64>,
    pre_start_state: &HashMap<Pubkey, usize>,
    committed_accounts: &[CommittedAccount],
) {
    verify_committed_accounts_state(rpc_client, committed_accounts).await;
    verify_buffers_cleaned_up(rpc_client, committed_accounts, commit_ids_by_pk)
        .await;
    verify_table_mania_released(table_mania, pre_start_state).await;
}
