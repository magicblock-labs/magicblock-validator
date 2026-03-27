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
use dlp_api::{args::CommitStateArgs, pda::ephemeral_balance_pda_from_payer};
use futures::future::{join_all, try_join_all};
use magicblock_committor_program::pdas;
use magicblock_committor_service::{
    intent_executor::{
        error::{IntentExecutorError, TransactionStrategyExecutionError},
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{
            CacheTaskInfoFetcher, RpcTaskInfoFetcher, TaskInfoFetcher,
            TaskInfoFetcherError,
        },
        two_stage_executor::{Initialized, TwoStageExecutor},
        utils::prepare_and_execute_strategy,
        ExecutionOutput, IntentExecutionReport, IntentExecutionResult,
        IntentExecutor, IntentExecutorImpl,
    },
    persist::IntentPersisterImpl,
    tasks::{
        task_builder::{TaskBuilderError, TaskBuilderImpl, TasksBuilder},
        task_strategist::{TaskStrategist, TransactionStrategy},
    },
    transaction_preparator::{
        TransactionPreparator, TransactionPreparatorImpl,
    },
    DEFAULT_ACTIONS_TIMEOUT,
};
use magicblock_core::{
    intent::{BaseActionCallback, CommittedAccount},
    traits::ActionError,
};
use magicblock_program::{
    args::ShortAccountMeta,
    magic_scheduled_base_intent::{
        BaseAction, CommitAndUndelegate, CommitType, MagicBaseIntent,
        MagicIntentBundle, ProgramArgs, ScheduledIntentBundle, UndelegateType,
    },
    validator::validator_authority_id,
};
use magicblock_rpc_client::MagicBlockSendTransactionConfig;
use magicblock_table_mania::TableMania;
use program_flexi_counter::{
    instruction::FlexiCounterInstruction,
    state::{FlexiCounter, FAIL_UNDELEGATION_LABEL},
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    instruction::InstructionError,
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};

use crate::{
    common::{MockActionsCallbackExecutor, TestFixture},
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
    task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    intent_executor: IntentExecutorImpl<
        TransactionPreparatorImpl,
        RpcTaskInfoFetcher,
        MockActionsCallbackExecutor,
    >,
    callback_executor: MockActionsCallbackExecutor,
    pre_test_tablemania_state: HashMap<Pubkey, usize>,
}

impl TestEnv {
    async fn setup() -> Self {
        let validator_auth = ensure_validator_authority();
        let fixture = TestFixture::new_with_keypair(validator_auth).await;
        fund_validator_auth_and_ensure_validator_fees_vault(&fixture.authority)
            .await;

        let transaction_preparator = fixture.create_transaction_preparator();
        let task_info_fetcher = Arc::new(CacheTaskInfoFetcher::new(
            RpcTaskInfoFetcher::new(fixture.rpc_client.clone()),
        ));

        let tm = &fixture.table_mania;
        let mut pre_test_tablemania_state = HashMap::new();
        for pubkey in tm.active_table_pubkeys().await {
            let count = tm.get_pubkey_refcount(&pubkey).await.unwrap_or(0);
            pre_test_tablemania_state.insert(pubkey, count);
        }

        let callback_executor = MockActionsCallbackExecutor::default();
        let intent_executor = IntentExecutorImpl::new(
            fixture.rpc_client.clone(),
            transaction_preparator,
            task_info_fetcher.clone(),
            callback_executor.clone(),
            DEFAULT_ACTIONS_TIMEOUT,
        );

        Self {
            fixture,
            task_info_fetcher,
            intent_executor,
            callback_executor,
            pre_test_tablemania_state,
        }
    }
}

#[tokio::test]
async fn test_commit_id_error_parsing() {
    const COUNTER_SIZE: u64 = 70;

    let TestEnv {
        fixture,
        intent_executor: _,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;
    let (counter_auth, account) = setup_counter(COUNTER_SIZE, None).await;
    let remote_slot = Default::default();

    let intent = create_intent(
        vec![CommittedAccount {
            pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
            account,
            remote_slot,
        }],
        true,
    );

    // Invalidate ids before execution
    task_info_fetcher
        .fetch_next_commit_nonces(
            &intent.get_undelegate_intent_pubkeys().unwrap(),
            remote_slot,
        )
        .await
        .unwrap();

    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &intent,
    )
    .await;
    let intent_client = IntentExecutionClient::new(fixture.rpc_client.clone());
    let transaction_preparator = fixture.create_transaction_preparator();
    let execution_result = prepare_and_execute_strategy(
        &intent_client,
        &fixture.authority,
        &transaction_preparator,
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
        TransactionStrategyExecutionError::CommitIDError(
            TransactionError::InstructionError(
                _,
                InstructionError::Custom(
                    0xc, // dlp_api::DlpError::NonceOutOfOrder: commit id/nonce is out of order
                )
            ),
            _
        )
    ));
    assert!(err
        .to_string()
        .contains("Accounts committed with an invalid Commit id"));
}

#[tokio::test]
async fn test_undelegation_error_parsing() {
    const COUNTER_SIZE: u64 = 70;

    let TestEnv {
        fixture,
        intent_executor: _,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    // Create counter that will force undelegation to fail
    let (counter_auth, account) =
        setup_counter(COUNTER_SIZE, Some(FAIL_UNDELEGATION_LABEL.to_string()))
            .await;
    let intent = create_intent(
        vec![CommittedAccount {
            pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
            account,
            remote_slot: Default::default(),
        }],
        true,
    );

    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &intent,
    )
    .await;
    let intent_client = IntentExecutionClient::new(fixture.rpc_client.clone());
    let transaction_preparator = fixture.create_transaction_preparator();
    let execution_result = prepare_and_execute_strategy(
        &intent_client,
        &fixture.authority,
        &transaction_preparator,
        &mut transaction_strategy,
        &None::<IntentPersisterImpl>,
    )
    .await;
    assert!(execution_result.is_ok(), "Preparation is expected to pass!");

    // Verify that we got UndelegationError
    let execution_result = execution_result.unwrap();
    assert!(execution_result.is_err());
    let err = execution_result.unwrap_err();
    assert!(matches!(
        err,
        TransactionStrategyExecutionError::UndelegationError(
            TransactionError::InstructionError(
                _,
                InstructionError::Custom(
                    0x7a, // flexi-counter ProgramError::Custom(122): forced undelegation failure (FAIL_UNDELEGATION_CODE)
                )
            ),
            _
        )
    ));
    assert!(err.to_string().contains("Invalid undelegation"));
}

#[tokio::test]
async fn test_action_error_parsing() {
    const COUNTER_SIZE: u64 = 70;

    let TestEnv {
        fixture,
        intent_executor: _,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    let (counter_auth, account) = setup_counter(COUNTER_SIZE, None).await;
    setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
        .await;

    let committed_account = CommittedAccount {
        pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
        account,
        remote_slot: Default::default(),
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
    let intent_client = IntentExecutionClient::new(fixture.rpc_client.clone());
    let transaction_preparator = fixture.create_transaction_preparator();
    let execution_result = prepare_and_execute_strategy(
        &intent_client,
        &fixture.authority,
        &transaction_preparator,
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
        TransactionStrategyExecutionError::ActionsError(
            TransactionError::InstructionError(
                _,
                InstructionError::ArithmeticOverflow
            ),
            _
        )
    ));
    assert!(execution_err
        .to_string()
        .contains("User supplied actions are ill-formed"));
}

#[tokio::test]
async fn test_cpi_limits_error_parsing() {
    const COUNTER_SIZE: u64 = 102;
    const COUNTER_NUM: u64 = 10;

    let TestEnv {
        fixture,
        intent_executor: _,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    let counters = (0..COUNTER_NUM).map(|_| async {
        let (counter_auth, account) = setup_counter(COUNTER_SIZE, None).await;
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
            remote_slot: Default::default(),
        })
        .collect();

    let scheduled_intent = create_intent(committed_accounts.clone(), true);
    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &scheduled_intent,
    )
    .await;
    let intent_client = IntentExecutionClient::new(fixture.rpc_client.clone());
    let transaction_preparator = fixture.create_transaction_preparator();
    let execution_result = prepare_and_execute_strategy(
        &intent_client,
        &fixture.authority,
        &transaction_preparator,
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
        TransactionStrategyExecutionError::CpiLimitError(
            TransactionError::InstructionError(
                _,
                InstructionError::MaxInstructionTraceLengthExceeded
            ),
            _
        )
    ));
    assert!(execution_err
        .to_string()
        .contains("Max instruction trace length exceeded"));
}

#[tokio::test]
async fn test_min_context_slot_not_reached_error_parsing() {
    const COUNTER_SIZE: u64 = 70;
    const EXPECTED_ERR_MSG: &str = "Minimum context slot";
    const REMOTE_SLOT: u64 = 1_000_000_000;

    let TestEnv {
        fixture: _,
        mut intent_executor,
        task_info_fetcher: _,
        callback_executor: _,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;
    let (counter_auth, account) = setup_counter(COUNTER_SIZE, None).await;

    let intent = create_intent(
        vec![CommittedAccount {
            pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
            account,
            remote_slot: REMOTE_SLOT,
        }],
        true,
    );

    let execution_result = intent_executor
        .execute(intent, None::<IntentPersisterImpl>)
        .await;

    // Verify that we got MinContextSlotNotReachedError
    assert!(execution_result.inner.is_err());
    let err = execution_result.inner.unwrap_err();
    assert!(
        matches!(
            err,
            IntentExecutorError::TaskBuilderError(
                TaskBuilderError::CommitTasksBuildError(
                    TaskInfoFetcherError::MinContextSlotNotReachedError(
                        REMOTE_SLOT,
                        _
                    )
                )
            )
        ),
        "err: {:?}",
        err
    );
    assert!(err.to_string().contains(EXPECTED_ERR_MSG));
}

#[tokio::test]
async fn test_commit_id_error_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        mut intent_executor,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let counter_auth = Keypair::new();
    let (pubkey, mut account) =
        init_and_delegate_account_on_chain(&counter_auth, COUNTER_SIZE, None)
            .await;

    account.owner = program_flexi_counter::id();
    let remote_slot = Default::default();
    let committed_account = CommittedAccount {
        pubkey,
        account,
        remote_slot,
    };
    let intent = create_intent(vec![committed_account.clone()], false);

    // Invalidate commit nonce cache
    let res = task_info_fetcher
        .fetch_next_commit_nonces(&[committed_account.pubkey], remote_slot)
        .await;
    assert!(res.is_ok());
    assert!(res.unwrap().contains_key(&committed_account.pubkey));

    // Now execute intent
    let res = intent_executor
        .execute(intent, None::<IntentPersisterImpl>)
        .await;
    let IntentExecutionResult {
        inner: res,
        patched_errors,
        callbacks_report,
    } = res;

    assert!(
        res.is_ok(),
        "res: {:?}, patched_errors: {:#?}",
        res,
        patched_errors
    );
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));
    assert!(callbacks_report.is_empty());

    assert_eq!(patched_errors.len(), 1, "Only 1 patch expected");
    let commit_id_error = patched_errors.into_iter().next().unwrap();
    assert!(matches!(
        commit_id_error,
        TransactionStrategyExecutionError::CommitIDError(_, _)
    ));

    // Cleanup succeeds
    assert!(intent_executor.cleanup().await.is_ok());
    let mut commit_ids_by_pk = HashMap::new();
    for el in [&committed_account].iter() {
        let nonce = task_info_fetcher
            .peek_commit_nonce(&el.pubkey)
            .await
            .unwrap();
        commit_ids_by_pk.insert(el.pubkey, nonce);
    }

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
async fn test_undelegation_error_recovery() {
    const COUNTER_SIZE: u64 = 70;

    let TestEnv {
        fixture,
        mut intent_executor,
        task_info_fetcher: _,
        callback_executor: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let counter_auth = Keypair::new();
    let (pubkey, mut account) = init_and_delegate_account_on_chain(
        &counter_auth,
        COUNTER_SIZE,
        Some(FAIL_UNDELEGATION_LABEL.to_string()),
    )
    .await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey,
        account,
        remote_slot: Default::default(),
    };
    let intent = create_intent(vec![committed_account.clone()], true);

    // Execute intent
    let res = intent_executor
        .execute(intent, None::<IntentPersisterImpl>)
        .await;
    let IntentExecutionResult {
        inner: res,
        patched_errors,
        callbacks_report,
    } = res;

    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));
    assert!(callbacks_report.is_empty());
    assert_eq!(patched_errors.len(), 1, "Only 1 patch expected");

    // Assert errors patched
    let undelegation_error = patched_errors.into_iter().next().unwrap();
    assert!(matches!(
        undelegation_error,
        TransactionStrategyExecutionError::UndelegationError(_, _)
    ));

    // Cleanup succeeds
    assert!(intent_executor.cleanup().await.is_ok());
    verify(
        &fixture.table_mania,
        fixture.rpc_client.get_inner(),
        &HashMap::new(),
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
        mut intent_executor,
        task_info_fetcher: _,
        callback_executor: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE, None).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
        remote_slot: Default::default(),
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
    let IntentExecutionResult {
        inner: res,
        patched_errors,
        callbacks_report,
    } = res;

    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));
    assert_eq!(callbacks_report.len(), 1, "1 callback from failing action");
    assert!(callbacks_report[0].is_ok(), "mock returns Ok(sig)");
    assert_eq!(patched_errors.len(), 1, "Only 1 patch expected");

    // Assert errors patched
    let action_error = patched_errors.into_iter().next().unwrap();
    assert!(matches!(
        action_error,
        TransactionStrategyExecutionError::ActionsError(_, _)
    ));

    verify_committed_accounts_state(
        fixture.rpc_client.get_inner(),
        &[committed_account],
    )
    .await;

    tokio::time::sleep(Duration::from_secs(1)).await;
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
        mut intent_executor,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE, None).await;

    account.owner = program_flexi_counter::id();
    let remote_slot = Default::default();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
        remote_slot,
    };

    // Invalidate commit nonce cache
    let res = task_info_fetcher
        .fetch_next_commit_nonces(&[committed_account.pubkey], remote_slot)
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
    // Execute intent
    let res = intent_executor
        .execute(scheduled_intent, None::<IntentPersisterImpl>)
        .await;
    let IntentExecutionResult {
        inner: res,
        patched_errors,
        callbacks_report,
    } = res;

    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));
    assert_eq!(callbacks_report.len(), 1, "1 callback from failing action");
    assert!(callbacks_report[0].is_ok(), "mock returns Ok(sig)");
    assert_eq!(patched_errors.len(), 2, "Only 2 patches expected");

    // Assert errors patched
    let mut iter = patched_errors.into_iter();
    let commit_id_error = iter.next().unwrap();
    let actions_error = iter.next().unwrap();
    assert!(matches!(
        commit_id_error,
        TransactionStrategyExecutionError::CommitIDError(_, _)
    ));
    assert!(matches!(
        actions_error,
        TransactionStrategyExecutionError::ActionsError(_, _)
    ));

    // Cleanup succeeds
    assert!(intent_executor.cleanup().await.is_ok());

    verify_committed_accounts_state(
        fixture.rpc_client.get_inner(),
        &[committed_account],
    )
    .await;

    tokio::time::sleep(Duration::from_secs(1)).await;
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
        mut intent_executor,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let counters = (0..COUNTER_NUM).map(|_| async {
        let (counter_auth, account) = setup_counter(COUNTER_SIZE, None).await;
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
                account,
                remote_slot: Default::default(),
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
    let mut execution_report = IntentExecutionReport::default();
    let execution_result = intent_executor
        .single_stage_execution_flow(
            scheduled_intent,
            strategy,
            &mut execution_report,
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

    // Cleanup after intent
    let transaction_preparator = fixture.create_transaction_preparator();
    let cleanup_futs = execution_report.junk().iter().map(|to_cleanup| {
        transaction_preparator.cleanup_for_strategy(
            &fixture.authority,
            &to_cleanup.optimized_tasks,
            &to_cleanup.lookup_tables_keys,
        )
    });
    assert!(try_join_all(cleanup_futs).await.is_ok());

    let mut commit_ids_by_pk = HashMap::new();
    for el in committed_accounts.iter() {
        let nonce = task_info_fetcher
            .peek_commit_nonce(&el.pubkey)
            .await
            .unwrap();
        commit_ids_by_pk.insert(el.pubkey, nonce);
    }

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
        mut intent_executor,
        task_info_fetcher,
        callback_executor: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    // Prepare multiple counters; each needs an escrow (payer) to be able to execute base actions.
    // We also craft unique on-chain data so we can verify post-commit state exactly.
    let counters = (0..COUNTER_NUM).map(|_| async {
        let (counter_auth, account) = setup_counter(COUNTER_SIZE, None).await;
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
                remote_slot: Default::default(),
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
        .fetch_next_commit_nonces(&pubkeys, Default::default())
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

    intent_executor.started_at = std::time::Instant::now();
    let mut execution_report = IntentExecutionReport::default();
    let res = intent_executor
        .single_stage_execution_flow(
            scheduled_intent,
            strategy,
            &mut execution_report,
            &None::<IntentPersisterImpl>,
        )
        .await;

    let patched_errors = execution_report.patched_errors();
    // We expect recovery to succeed by splitting into two stages (commit, then finalize)
    assert!(
        res.is_ok(),
        "Expected recovery from CommitID, Actions, and CpiLimit errors"
    );
    assert_eq!(patched_errors.len(), 3, "Only 3 patches expected");
    assert!(matches!(
        res.unwrap(),
        ExecutionOutput::TwoStage {
            commit_signature: _,
            finalize_signature: _,
        }
    ));

    let mut iter = patched_errors.iter();
    let commit_id_error = iter.next().unwrap();
    let cpi_limit_err = iter.next().unwrap();
    let action_error = iter.next().unwrap();

    assert!(matches!(
        commit_id_error,
        TransactionStrategyExecutionError::CommitIDError(_, _)
    ));
    assert!(matches!(
        cpi_limit_err,
        TransactionStrategyExecutionError::CpiLimitError(_, _)
    ));
    assert!(matches!(
        action_error,
        TransactionStrategyExecutionError::ActionsError(_, _)
    ));

    // Cleanup after intent
    let transaction_preparator = fixture.create_transaction_preparator();
    let cleanup_futs = execution_report.junk().iter().map(|to_cleanup| {
        transaction_preparator.cleanup_for_strategy(
            &fixture.authority,
            &to_cleanup.optimized_tasks,
            &to_cleanup.lookup_tables_keys,
        )
    });
    assert!(try_join_all(cleanup_futs).await.is_ok());
    let mut commit_ids_by_pk = HashMap::new();
    for el in committed_accounts.iter() {
        let nonce = task_info_fetcher
            .peek_commit_nonce(&el.pubkey)
            .await
            .unwrap();
        commit_ids_by_pk.insert(el.pubkey, nonce);
    }
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

#[tokio::test]
async fn test_commit_unfinalized_account_recovery() {
    let TestEnv {
        fixture,
        mut intent_executor,
        task_info_fetcher: _,
        callback_executor: _,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    // Prepare multiple counters; each needs an escrow (payer) to be able to execute base actions.
    // We also craft unique on-chain data so we can verify post-commit state exactly.
    let (counter_auth, account) = setup_counter(40, None).await;
    setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
        .await;
    let pda = FlexiCounter::pda(&counter_auth.pubkey()).0;

    // Commit account without finalization
    // This simulates finalization stage failure
    {
        let commit_allow_undelegation_ix =
            dlp_api::instruction_builder::commit_state(
                fixture.authority.pubkey(),
                pda,
                account.owner,
                CommitStateArgs {
                    nonce: 1,
                    lamports: account.lamports,
                    allow_undelegation: false,
                    data: account.data.clone(),
                },
            );

        let blockhash =
            fixture.rpc_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[commit_allow_undelegation_ix],
            Some(&fixture.authority.pubkey()),
            &[&fixture.authority],
            blockhash,
        );

        let result = fixture
            .rpc_client
            .send_transaction(
                &tx,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await;
        assert!(result.is_ok());
    }

    // Now simulate user sending new intent
    let committed_account = CommittedAccount {
        pubkey: pda,
        account,
        remote_slot: Default::default(),
    };
    let intent = create_intent(vec![committed_account], false);
    let result = intent_executor
        .execute(intent, None::<IntentPersisterImpl>)
        .await;
    assert!(result.inner.is_ok());
    assert!(matches!(
        result.inner.unwrap(),
        ExecutionOutput::SingleStage(_)
    ));

    assert_eq!(result.patched_errors.len(), 2);
    assert!(matches!(
        result.patched_errors[0],
        TransactionStrategyExecutionError::UnfinalizedAccountError(_, _)
    ));
    assert!(matches!(
        result.patched_errors[1],
        TransactionStrategyExecutionError::CommitIDError(_, _)
    ))
}

#[tokio::test]
async fn test_commit_unfinalized_account_recovery_two_stage() {
    let TestEnv {
        fixture,
        mut intent_executor,
        task_info_fetcher: _,
        callback_executor: _,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    // Prepare multiple counters; each needs an escrow (payer) to be able to execute base actions.
    // We also craft unique on-chain data so we can verify post-commit state exactly.
    let counters = (0..8).map(async |_| {
        let (counter_auth, account) = setup_counter(40, None).await;
        setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
            .await;
        let pda = FlexiCounter::pda(&counter_auth.pubkey()).0;
        (counter_auth, pda, account)
    });
    let counters: Vec<(_, _, _)> = join_all(counters).await;

    // Commit account without finalization
    // This simulates finalization stage failure
    {
        let commit_allow_undelegation_ix =
            dlp_api::instruction_builder::commit_state(
                fixture.authority.pubkey(),
                counters[0].1,
                counters[0].2.owner,
                CommitStateArgs {
                    nonce: 1,
                    lamports: counters[0].2.lamports,
                    allow_undelegation: false,
                    data: counters[0].2.data.clone(),
                },
            );

        let blockhash =
            fixture.rpc_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[commit_allow_undelegation_ix],
            Some(&fixture.authority.pubkey()),
            &[&fixture.authority],
            blockhash,
        );

        let result = fixture
            .rpc_client
            .send_transaction(
                &tx,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await;
        assert!(result.is_ok());
    }

    // Now simulate user sending new intent
    let committed_accounts = counters
        .into_iter()
        .map(|el| CommittedAccount {
            pubkey: el.1,
            account: el.2,
            remote_slot: Default::default(),
        })
        .collect();
    let intent = create_intent(committed_accounts, true);

    let result = intent_executor
        .execute(intent, None::<IntentPersisterImpl>)
        .await;
    assert!(result.inner.is_ok());
    assert!(matches!(
        result.inner.unwrap(),
        ExecutionOutput::TwoStage {
            commit_signature: _,
            finalize_signature: _
        }
    ));

    assert_eq!(result.patched_errors.len(), 2);
    assert!(matches!(
        result.patched_errors[0],
        TransactionStrategyExecutionError::UnfinalizedAccountError(_, _)
    ));
    assert!(matches!(
        result.patched_errors[1],
        TransactionStrategyExecutionError::CommitIDError(_, _)
    ))
}

#[tokio::test]
async fn test_action_callback_fired_on_failure() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        mut intent_executor,
        task_info_fetcher: _,
        callback_executor,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE, None).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
        remote_slot: Default::default(),
    };

    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action =
        failing_undelegate_action(payer.pubkey(), committed_account.pubkey);
    let UndelegateType::WithBaseActions(ref actions) = undelegate_action else {
        panic!("expected base actions");
    };
    let expected_callback = actions[0].callback.clone().unwrap();

    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    let scheduled_intent = create_scheduled_intent(base_intent);
    let res = intent_executor
        .execute(scheduled_intent, None::<IntentPersisterImpl>)
        .await;

    assert!(res.inner.is_ok());
    assert_eq!(res.callbacks_report.len(), 1, "1 callback scheduled");
    assert!(res.callbacks_report[0].is_ok(), "mock returns Ok(sig)");

    let calls = callback_executor.calls();
    assert_eq!(calls.len(), 1);
    let (callbacks, result) = &calls[0];
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0], expected_callback);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_action_callback_fired_on_timeout() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        intent_executor: _,
        task_info_fetcher: _,
        callback_executor: _,
        pre_test_tablemania_state,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE, None).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
        remote_slot: Default::default(),
    };

    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action =
        failing_undelegate_action(payer.pubkey(), committed_account.pubkey);
    let UndelegateType::WithBaseActions(ref actions) = undelegate_action else {
        panic!("expected base actions");
    };
    let expected_callback = actions[0].callback.clone().unwrap();

    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    // Build executor with zero timeout so actions time out before execution
    let callback_executor = MockActionsCallbackExecutor::default();
    let task_info_fetcher = Arc::new(CacheTaskInfoFetcher::new(
        RpcTaskInfoFetcher::new(fixture.rpc_client.clone()),
    ));
    let mut intent_executor = IntentExecutorImpl::new(
        fixture.rpc_client.clone(),
        fixture.create_transaction_preparator(),
        task_info_fetcher,
        callback_executor.clone(),
        Duration::ZERO,
    );

    let scheduled_intent = create_scheduled_intent(base_intent);
    let res = intent_executor
        .execute(scheduled_intent, None::<IntentPersisterImpl>)
        .await;

    assert!(res.inner.is_ok());
    assert!(res.patched_errors.is_empty());
    assert_eq!(res.callbacks_report.len(), 1, "1 callback scheduled");
    assert!(res.callbacks_report[0].is_ok(), "mock returns Ok(sig)");

    let calls = callback_executor.calls();
    assert_eq!(calls.len(), 1);
    let (callbacks, result) = &calls[0];
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0], expected_callback);
    assert!(matches!(result, Err(ActionError::TimeoutError)));

    assert!(intent_executor.cleanup().await.is_ok());
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
async fn test_callbacks_fired_in_two_stage() {
    const COUNTER_SIZE: u64 = 70;

    let TestEnv {
        fixture,
        intent_executor: _,
        task_info_fetcher,
        callback_executor,
        pre_test_tablemania_state: _,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE, None).await;
    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
        remote_slot: Default::default(),
    };

    // commit-stage action: goes into standalone_actions → commit strategy
    let commit_base_action =
        succeeding_commit_action(payer.pubkey(), counter_pubkey);
    let expected_commit_callback = commit_base_action.callback.clone().unwrap();

    // finalize-stage action: goes into UndelegateType::WithBaseActions → finalize strategy
    let undelegate_action =
        succeeding_undelegate_action(payer.pubkey(), counter_pubkey);
    let UndelegateType::WithBaseActions(ref actions) = undelegate_action else {
        panic!("expected base actions");
    };
    let expected_finalize_callback = actions[0].callback.clone().unwrap();

    let intent = create_scheduled_intent_from_bundle(MagicIntentBundle {
        commit_and_undelegate: Some(CommitAndUndelegate {
            commit_action: CommitType::Standalone(vec![
                committed_account.clone()
            ]),
            undelegate_action,
        }),
        standalone_actions: vec![commit_base_action],
        ..Default::default()
    });
    let committed_pubkeys = intent.get_all_committed_pubkeys();

    let transaction_preparator = fixture.create_transaction_preparator();
    let mut execution_report = IntentExecutionReport::default();
    let mut executor = create_two_stage_executor(
        &fixture,
        &callback_executor,
        &intent,
        &task_info_fetcher,
        &mut execution_report,
    )
    .await;

    // Execute commit stage
    let commit_sig = executor
        .commit(
            &committed_pubkeys,
            &transaction_preparator,
            &task_info_fetcher,
            &None::<IntentPersisterImpl>,
        )
        .await
        .expect("commit must succeed");
    executor.execute_callbacks(Ok::<(), ActionError>(()));

    // standalone_actions land in the commit strategy as a BaseAction
    let calls = callback_executor.calls();
    assert_eq!(
        calls.len(),
        1,
        "commit-stage callback must be fired by execute_callbacks"
    );
    assert_eq!(calls[0].0[0], expected_commit_callback);
    assert!(calls[0].1.is_ok());

    // Execute finalize stage
    let mut finalize_executor = executor.done(commit_sig);
    finalize_executor
        .finalize(&transaction_preparator, &None::<IntentPersisterImpl>)
        .await
        .expect("finalize must succeed");
    finalize_executor.execute_callbacks(Ok::<(), ActionError>(()));

    // Expect 2 actions to be executed in finalize stage
    let calls = callback_executor.calls();
    assert_eq!(
        calls.len(),
        2,
        "finalize-stage callback must be fired by execute_callbacks"
    );
    assert_eq!(calls[1].0[0], expected_finalize_callback);
    assert!(calls[1].1.is_ok());
}

/// Builds a [`TwoStageExecutor`] directly from an intent by constructing the
/// commit and finalize strategies independently, without going through
/// `execute_inner` or any CPI-limit recovery path.
async fn create_two_stage_executor<'a>(
    fixture: &TestFixture,
    callback_executor: &MockActionsCallbackExecutor,
    intent: &ScheduledIntentBundle,
    task_info_fetcher: &Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    execution_report: &'a mut IntentExecutionReport,
) -> TwoStageExecutor<'a, MockActionsCallbackExecutor, Initialized> {
    let authority = &fixture.authority.pubkey();
    let commit_tasks = TaskBuilderImpl::commit_tasks(
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
    let commit_strategy = TaskStrategist::build_strategy(
        commit_tasks,
        authority,
        &None::<IntentPersisterImpl>,
    )
    .unwrap();
    let finalize_strategy = TaskStrategist::build_strategy(
        finalize_tasks,
        authority,
        &None::<IntentPersisterImpl>,
    )
    .unwrap();
    TwoStageExecutor::new(
        fixture.authority.insecure_clone(),
        commit_strategy,
        finalize_strategy,
        IntentExecutionClient::new(fixture.rpc_client.clone()),
        callback_executor.clone(),
        execution_report,
    )
}

fn succeeding_commit_action(
    escrow_authority: Pubkey,
    account: Pubkey,
) -> BaseAction {
    const PRIZE: u64 = 900_000;

    let data = FlexiCounterInstruction::CommitActionHandler { amount: PRIZE };
    let transfer_destination = Pubkey::new_unique();
    let account_metas = vec![
        ShortAccountMeta {
            pubkey: account,
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
    BaseAction {
        compute_units: 100_000,
        destination_program: program_flexi_counter::id(),
        source_program: Some(program_flexi_counter::id()),
        escrow_authority,
        data_per_program: ProgramArgs {
            escrow_index: ACTOR_ESCROW_INDEX,
            data: to_vec(&data).unwrap(),
        },
        account_metas_per_program: account_metas,
        callback: Some(create_callback()),
    }
}

fn succeeding_undelegate_action(
    escrow_authority: Pubkey,
    undelegated_account: Pubkey,
) -> UndelegateType {
    const PRIZE: u64 = 1_000_000;
    const SUCCESS_DIFF: i64 = 1; // positive, no underflow on a zero-initialised counter

    let undelegate_action_data =
        FlexiCounterInstruction::UndelegateActionHandler {
            counter_diff: SUCCESS_DIFF,
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
        source_program: Some(program_flexi_counter::id()),
        escrow_authority,
        data_per_program: ProgramArgs {
            escrow_index: ACTOR_ESCROW_INDEX,
            data: to_vec(&undelegate_action_data).unwrap(),
        },
        account_metas_per_program: account_metas,
        callback: Some(create_callback()),
    }])
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
        source_program: Some(program_flexi_counter::id()),
        escrow_authority,
        data_per_program: ProgramArgs {
            escrow_index: ACTOR_ESCROW_INDEX,
            data: to_vec(&undelegate_action_data).unwrap(),
        },
        account_metas_per_program: account_metas,
        callback: Some(create_callback()),
    }])
}

fn create_callback() -> BaseActionCallback {
    BaseActionCallback {
        destination_program: program_flexi_counter::id(),
        discriminator: vec![1, 2, 3, 4, 5, 6, 7, 8],
        payload: vec![],
        compute_units: 50_000,
        account_metas_per_program: vec![],
    }
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
    let ix = dlp_api::instruction_builder::top_up_ephemeral_balance(
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

async fn setup_counter(
    counter_bytes: u64,
    label: Option<String>,
) -> (Keypair, Account) {
    let counter_auth = Keypair::new();
    let (_, mut account) =
        init_and_delegate_account_on_chain(&counter_auth, counter_bytes, label)
            .await;

    account.owner = program_flexi_counter::id();
    (counter_auth, account)
}

fn create_intent(
    committed_accounts: Vec<CommittedAccount>,
    is_undelegate: bool,
) -> ScheduledIntentBundle {
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
) -> ScheduledIntentBundle {
    create_scheduled_intent_from_bundle(base_intent.into())
}

fn create_scheduled_intent_from_bundle(
    intent_bundle: MagicIntentBundle,
) -> ScheduledIntentBundle {
    static INTENT_ID: AtomicU64 = AtomicU64::new(0);

    ScheduledIntentBundle {
        id: INTENT_ID.fetch_add(1, Ordering::Relaxed),
        slot: 10,
        blockhash: Hash::new_unique(),
        sent_transaction: Transaction::default(),
        payer: Pubkey::new_unique(),
        intent_bundle,
    }
}

async fn single_flow_transaction_strategy(
    authority: &Pubkey,
    task_info_fetcher: &Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    intent: &ScheduledIntentBundle,
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
