mod common;
use std::{
    sync::{Arc, Mutex, Once},
    time::Duration,
};

use anyhow::anyhow;
use async_trait::async_trait;
use common::*;
use integration_test_tools::{
    loaded_accounts::DLP_TEST_AUTHORITY_BYTES, IntegrationTestContext,
};
use magicblock_committor_service::{
    intent_executor::{
        accepted_intent_executor::AcceptedIntentExecutor,
        build_stage_intent_executor, error::IntentExecutorResult,
        intent_execution_client::IntentExecutionClient, ExecutionOutput,
        IntentExecutionReport, IntentExecutor, IntentExecutorCtx,
    },
    outbox::{
        outbox_client::InternalOutboxClientError,
        outbox_intent_bundles_reader::OutboxIntentBundlesReader, OutboxClient,
        ScheduledBaseIntentMeta,
    },
    tasks::task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
    transaction_preparator::TransactionPreparatorImpl,
    ComputeBudgetConfig, DEFAULT_ACTIONS_TIMEOUT,
};
use magicblock_core::{
    intent::{outbox::outbox_intent_pda, BaseActionCallback},
    traits::{
        ActionError, ActionResult, ActionsCallbackScheduler,
        CallbackScheduleError,
    },
};
use magicblock_magic_program_api::{
    args::{
        CommitAndUndelegateArgs, CommitTypeArgs, MagicIntentBundleArgs,
        UndelegateTypeArgs,
    },
    instruction::MagicBlockInstruction,
    outbox::{ExecutionStage, TwoStageProgress},
    MAGIC_CONTEXT_PUBKEY,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent_bundles::{OutboxIntentBundle, OutboxIntentBundleStatus},
    validator::init_validator_authority,
    MagicContext,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use program_flexi_counter::{
    instruction::create_transfer_intent_ix, state::FlexiCounter,
};
use serial_test::serial;
use solana_rpc_client::{
    http_sender::HttpSender,
    nonblocking::rpc_client::RpcClient as AsyncRpcClient,
    rpc_client::RpcClientConfig,
    rpc_sender::{RpcSender, RpcTransportStats},
};
use solana_rpc_client_api::{client_error, request::RpcRequest};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

type CallbackRecord =
    (Vec<BaseActionCallback>, Option<Signature>, ActionResult);

type TestIntentExecutorCtx = IntentExecutorCtx<
    TransactionPreparatorImpl,
    RpcTaskInfoFetcher,
    RecordingCallbackScheduler,
    TestOutboxClient,
>;

struct TestEnv {
    ctx: IntegrationTestContext,
    validator_authority: Keypair,
    chain_mb_client: MagicblockRpcClient,
    ephem_rpc: Arc<AsyncRpcClient>,
    intent_client: IntentExecutionClient,
    table_mania: TableMania,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    callback_scheduler: RecordingCallbackScheduler,
}

impl TestEnv {
    async fn setup() -> Self {
        let validator_authority = ensure_validator_authority();
        let ctx = IntegrationTestContext::try_new().unwrap();
        let chain_rpc = Arc::new(ctx.try_chain_client_async().unwrap());
        let ephem_rpc = Arc::new(ctx.try_ephem_client_async().unwrap());
        let chain_mb_rpc = MagicblockRpcClient::new(chain_rpc);

        let intent_client = IntentExecutionClient::new(chain_mb_rpc.clone());
        let gc_config = GarbageCollectorConfig::default();
        let table_mania = TableMania::new(
            chain_mb_rpc.clone(),
            &validator_authority,
            Some(gc_config),
        );
        let task_info_fetcher = Arc::new(CacheTaskInfoFetcher::new(
            RpcTaskInfoFetcher::new(chain_mb_rpc.clone()),
        ));

        Self {
            ctx,
            validator_authority,
            chain_mb_client: chain_mb_rpc,
            ephem_rpc,
            intent_client,
            table_mania,
            task_info_fetcher,
            callback_scheduler: RecordingCallbackScheduler::default(),
        }
    }

    fn executor_ctx_builder(&self) -> TestIntentExecutorCtxBuilder {
        let ctx = TestIntentExecutorCtx {
            intent_client: self.intent_client.clone(),
            transaction_preparator: self.transaction_preparator(),
            task_info_fetcher: self.task_info_fetcher.clone(),
            outbox_client: self.outbox_client().into(),
            actions_callback_executor: self.callback_scheduler.clone(),
        };
        TestIntentExecutorCtxBuilder { ctx }
    }

    fn outbox_client(&self) -> TestOutboxClient {
        TestOutboxClient::new(
            self.ephem_rpc.clone(),
            self.validator_authority.insecure_clone(),
        )
    }

    fn transaction_preparator(&self) -> TransactionPreparatorImpl {
        let compute_budget = ComputeBudgetConfig::new(1_000_000);
        TransactionPreparatorImpl::new(
            self.chain_mb_client.clone(),
            self.table_mania.clone(),
            compute_budget,
        )
    }

    fn intent_client_with_send_sleep(
        &self,
        sleep_duration: Duration,
    ) -> IntentExecutionClient {
        let sender = SleepyRpcSender {
            inner: HttpSender::new(IntegrationTestContext::url_chain()),
            sleep_duration,
        };
        let rpc_client = Arc::new(AsyncRpcClient::new_sender(
            sender,
            RpcClientConfig::with_commitment(self.ctx.commitment),
        ));
        IntentExecutionClient::new(MagicblockRpcClient::new(rpc_client))
    }
}

pub struct TestIntentExecutorCtxBuilder {
    ctx: TestIntentExecutorCtx,
}

impl TestIntentExecutorCtxBuilder {
    fn with_outbox_client(mut self, value: Arc<TestOutboxClient>) -> Self {
        self.ctx.outbox_client = value;
        self
    }

    fn with_intent_client(mut self, value: IntentExecutionClient) -> Self {
        self.ctx.intent_client = value;
        self
    }

    fn build(self) -> TestIntentExecutorCtx {
        self.ctx
    }
}

/// Schedyles commit of counters directly via magic-program using validator authority
fn schedule_commit_finalize(ctx: &IntegrationTestContext, counters: &[Pubkey]) {
    let validator_keypair = ensure_validator_authority();

    let schedule_ix = schedule_commit_instruction(
        &validator_keypair.pubkey(),
        counters.to_vec(),
    );
    let mut tx = Transaction::new_with_payer(
        &[schedule_ix],
        Some(&validator_keypair.pubkey()),
    );
    let sig = ctx
        .send_transaction_ephem(&mut tx, &[&validator_keypair])
        .unwrap();
    println!("schedule_commit_finalize sig: {}", sig);
}

fn schedule_intent_with_callback(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    destination: Pubkey,
    amount: u64,
) {
    let schedule_ix = create_transfer_intent_ix(
        payer.pubkey(),
        destination,
        ctx.ephem_validator_identity.unwrap(),
        amount,
        false,
        100_000,
    );
    let mut tx =
        Transaction::new_with_payer(&[schedule_ix], Some(&payer.pubkey()));
    let sig = ctx.send_transaction_ephem(&mut tx, &[payer]).unwrap();

    println!("schedule_intent_with_callback sig: {}", sig);
}

/// Tries to accept intent
fn steal_accept_intent(
    ctx: &IntegrationTestContext,
    intent_id: u64,
) -> Result<(), anyhow::Error> {
    let validator_keypair = ensure_validator_authority();

    let accept_ix =
        InstructionUtils::accept_scheduled_commits_instruction([intent_id]);
    let mut tx = Transaction::new_with_payer(
        &[accept_ix],
        Some(&validator_keypair.pubkey()),
    );

    let (sig, confirmed) =
        ctx.send_and_confirm_transaction_ephem(&mut tx, &[&validator_keypair])?;

    if !confirmed {
        Err(anyhow!("tx not confirmed: {}", sig))
    } else {
        println!("accept sig: {}", sig);
        Ok(())
    }
}

fn steal_schedule_accept_intent(
    ctx: &IntegrationTestContext,
    schedule: impl Fn(&IntegrationTestContext),
) -> u64 {
    const MAX_ATTEMPTS: u8 = 5;

    let mut attempt = 0;
    loop {
        let intent_id = read_next_intent_id(ctx);
        schedule(ctx);
        let result = steal_accept_intent(&ctx, intent_id);
        match result {
            Ok(()) => return intent_id,
            Err(err) => {
                println!("Failed to steal");

                if attempt >= MAX_ATTEMPTS {
                    panic!("Failed to steal intent: {}", err);
                }
                attempt += 1;
            }
        }
    }
}

fn schedule_and_accept(
    ctx: &IntegrationTestContext,
    schedule: impl Fn(&IntegrationTestContext),
) -> OutboxIntentBundle {
    ctx.wait_for_next_slot_ephem().unwrap();

    let intent_id = steal_schedule_accept_intent(&ctx, schedule);
    let pda = outbox_intent_pda(intent_id);
    let data = ctx.fetch_ephem_account_data(pda).unwrap();
    OutboxIntentBundle::try_from_bytes(&data).unwrap()
}

/// AcceptedIntentExecutor drives the full commit flow using TestOutboxClient.
/// Verifies: executor succeeds, set_intent_execution_stage is called,
/// and the committed value lands on the base chain.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pickup_executed_intent() {
    let test_env = TestEnv::setup().await;

    let payer = setup_payer(&test_env.ctx);
    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;

    init_counter(&test_env.ctx, &payer);
    delegate_counter(&test_env.ctx, &payer);
    add_to_counter(&test_env.ctx, &payer, 42);

    let outbox_bundle = schedule_and_accept(&test_env.ctx, |ctx| {
        schedule_commit_finalize(ctx, &[counter_pda])
    });

    // Execute intent
    let executor_ctx = test_env.executor_ctx_builder().build();
    let executor =
        AcceptedIntentExecutor::new(executor_ctx, DEFAULT_ACTIONS_TIMEOUT);
    let (result, cleanup_handle) = Box::new(executor)
        .execute(outbox_bundle.inner.clone())
        .await;
    assert!(result.inner.is_ok(), "Executor failed: {:?}", result.inner);

    cleanup_handle.clean().await.expect("cleanup failed");

    // Simulates shutdown/crash after successful execution
    // Validator could sent tx and then crash
    // On restart validator will extract outbox bundles
    let outbox_bundle = test_env
        .outbox_client()
        .outbox_reader()
        .fetch_outbox_intent(outbox_bundle.inner.id)
        .await
        .expect("fetch failed")
        .expect("outbox bundle not found");

    let ExecutionOutput::SingleStage(signature) = result.inner.unwrap() else {
        panic!("Unexpected execution strategy");
    };
    assert!(
        matches!(
            outbox_bundle.status,
            OutboxIntentBundleStatus::Executing(ExecutionStage::SingleStage(
                signature
            ))
        ),
        "Invalid outbox state"
    );
    // Builder executor
    let executor = build_stage_intent_executor(
        test_env.executor_ctx_builder().build(),
        outbox_bundle.status,
        DEFAULT_ACTIONS_TIMEOUT,
    );
    let (result, cleanup_handle) = Box::new(executor)
        .execute(outbox_bundle.inner.clone())
        .await;
    let ExecutionOutput::SingleStage(retried_signature) = result.inner.unwrap()
    else {
        panic!("Unexpected execution strategy");
    };
    assert_eq!(
        signature, retried_signature,
        "Execution shouldn't be retried"
    );
    cleanup_handle.clean().await.expect("cleanup failed");

    // Validate on chain state
    let counter = test_env
        .ctx
        .fetch_chain_account_struct::<FlexiCounter>(counter_pda)
        .unwrap();
    assert_eq!(counter.count, 42);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pickup_failed_intent() {
    let test_env = TestEnv::setup().await;

    let payer = setup_payer(&test_env.ctx);
    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;

    init_counter(&test_env.ctx, &payer);
    delegate_counter(&test_env.ctx, &payer);
    add_to_counter(&test_env.ctx, &payer, 42);
    let outbox_bundle = schedule_and_accept(&test_env.ctx, |ctx| {
        schedule_commit_finalize(ctx, &[counter_pda]);
    });
    let intent_id = outbox_bundle.id;

    // Create executor that will fail before chain execution
    let mut failing_outbox = test_env.outbox_client();
    failing_outbox.with_fail_execution_stage(true);

    let executor_ctx = test_env
        .executor_ctx_builder()
        .with_outbox_client(failing_outbox.into())
        .build();

    let executor = Box::new(AcceptedIntentExecutor::new(
        executor_ctx,
        DEFAULT_ACTIONS_TIMEOUT,
    ));
    let (result, cleanup_handle) = executor.execute(outbox_bundle.inner).await;
    assert!(result
        .inner
        .unwrap_err()
        .to_string()
        .contains("set_intent_execution_stage failed"));
    cleanup_handle
        .clean()
        .await
        .expect("Fail must succeed even after failure");

    // Verify chain status is still accepted
    let chain_outbox_bundle = test_env
        .outbox_client()
        .outbox_reader()
        .fetch_outbox_intent(intent_id)
        .await
        .expect("fetch suceeds")
        .expect("outbox intent exists");
    assert_eq!(
        chain_outbox_bundle.status,
        OutboxIntentBundleStatus::Accepted,
        "Invalid outbox state"
    );

    // Build executor that will drive execution to success
    let executor_ctx = test_env.executor_ctx_builder().build();
    let executor = Box::new(AcceptedIntentExecutor::new(
        executor_ctx,
        DEFAULT_ACTIONS_TIMEOUT,
    ));
    let (result, cleanup_handle) =
        executor.execute(chain_outbox_bundle.inner).await;
    let ExecutionOutput::SingleStage(_) =
        result.inner.expect("execution succeeded")
    else {
        panic!("Unexpected execution strategy");
    };
    cleanup_handle.clean().await.expect("cleanup failed");

    // Validate on chain state
    let counter = test_env
        .ctx
        .fetch_chain_account_struct::<FlexiCounter>(counter_pda)
        .unwrap();
    assert_eq!(counter.count, 42);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pickup_after_timeout() {
    const ACTIONS_TIMEOUT: Duration = Duration::from_millis(15);
    /// Larger than `ACTIONS_TIMEOUT` in order to trigger timeout int `execute_with_timeout`
    const SET_EXECUTION_STAGE_SLEEP: Duration = Duration::from_millis(20);
    const TRANSFER_AMOUNT: u64 = 900_000;

    let test_env = TestEnv::setup().await;

    let payer = setup_payer(&test_env.ctx);
    let chain_payer = setup_payer(&test_env.ctx);

    init_counter(&test_env.ctx, &payer);
    delegate_counter(&test_env.ctx, &payer);
    add_to_counter(&test_env.ctx, &payer, 42);
    test_env.ctx.delegate_account(&chain_payer, &payer).unwrap();

    let destination = Keypair::new();
    let payer_balance_before = test_env
        .ctx
        .fetch_ephem_account_balance(&payer.pubkey())
        .unwrap();
    let outbox_bundle = schedule_and_accept(&test_env.ctx, |ctx| {
        schedule_intent_with_callback(
            ctx,
            &payer,
            destination.pubkey(),
            TRANSFER_AMOUNT,
        );
    });

    let mut slow_outbox_client = test_env.outbox_client();
    slow_outbox_client
        .with_set_execution_stage_sleep(SET_EXECUTION_STAGE_SLEEP);
    let executor_ctx = test_env
        .executor_ctx_builder()
        .with_outbox_client(slow_outbox_client.into())
        .build();

    let executor =
        Box::new(AcceptedIntentExecutor::new(executor_ctx, ACTIONS_TIMEOUT));
    let (result, cleanup_handle) =
        executor.execute(outbox_bundle.inner.clone()).await;
    assert!(result.inner.is_ok(), "Executor failed: {:?}", result.inner);
    cleanup_handle.clean().await.expect("cleanup failed");

    // Callback was triggered with TimeoutError (set_intent_execution_stage exceeded ACTIONS_TIMEOUT)
    let callback_calls = test_env.callback_scheduler.recorded_calls();
    assert_eq!(
        callback_calls.len(),
        1,
        "Expected exactly one callback invocation"
    );
    let (callbacks, signature, cb_result) = &callback_calls[0];
    assert!(!callbacks.is_empty(), "Callback list must not be empty");
    assert!(signature.is_none(), "No signature on timeout");
    assert!(
        matches!(cb_result, Err(ActionError::TimeoutError)),
        "Expected TimeoutError, got: {:?}",
        cb_result
    );

    // Despite the callback timeout the commit tx still lands on chain
    let payer_balance_after = test_env
        .ctx
        .fetch_ephem_account_balance(&payer.pubkey())
        .unwrap();
    assert_eq!(
        payer_balance_after + BASE_ACTION_FEE + CALLBACK_FEE + TRANSFER_AMOUNT,
        payer_balance_before,
        "Payer fees not deducted correctly"
    );
    let dest_balance = test_env
        .ctx
        .fetch_chain_account_balance(&destination.pubkey())
        .unwrap();
    assert_eq!(
        dest_balance, TRANSFER_AMOUNT,
        "Destination did not receive funds"
    );
}

/// Similar to `test_pickup_after_timeout` but the slow-down is at the RpcSender
/// level rather than at `set_intent_execution_stage`.
///
/// Timeline inside `stage_execution_loop`:
///   1. getLatestBlockhash   → HTTP response + SEND_SLEEP (~80ms) → ok
///   2. set_intent_execution_stage (TestOutboxClient, instant)
///   3. pending_signature = Some(sig)
///   4. sendTransaction      → HTTP response (tx IS on chain) + SEND_SLEEP starts
///                            → ACTIONS_TIMEOUT fires during that sleep
///
/// ACTIONS_TIMEOUT (100ms) sits between one SEND_SLEEP (80ms) and two (160ms),
/// so the tx is always submitted before the timeout, but the callback gets
/// TimeoutError with no signature.  On re-execution the executor finds
/// pending_signature set, confirms the tx succeeded, and returns without
/// re-submitting.
/// TODO(edwin): add same but where tx wasn't actually sent and we timedout
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pick_up_after_tx_submission() {
    const SEND_SLEEP: Duration = Duration::from_millis(80);
    const ACTIONS_TIMEOUT: Duration = Duration::from_millis(100);
    const TRANSFER_AMOUNT: u64 = 900_000;

    let test_env = TestEnv::setup().await;

    let payer = setup_payer(&test_env.ctx);
    let chain_payer = setup_payer(&test_env.ctx);

    init_counter(&test_env.ctx, &payer);
    delegate_counter(&test_env.ctx, &payer);
    add_to_counter(&test_env.ctx, &payer, 42);
    test_env.ctx.delegate_account(&chain_payer, &payer).unwrap();

    let destination = Keypair::new();
    let payer_balance_before = test_env
        .ctx
        .fetch_ephem_account_balance(&payer.pubkey())
        .unwrap();
    let outbox_bundle = schedule_and_accept(&test_env.ctx, |ctx| {
        schedule_intent_with_callback(
            ctx,
            &payer,
            destination.pubkey(),
            TRANSFER_AMOUNT,
        );
    });

    let slow_intent_client = test_env.intent_client_with_send_sleep(SEND_SLEEP);
    let executor_ctx = test_env
        .executor_ctx_builder()
        .with_intent_client(slow_intent_client)
        .build();

    let executor =
        Box::new(AcceptedIntentExecutor::new(executor_ctx, ACTIONS_TIMEOUT));
    let (result, cleanup_handle) =
        executor.execute(outbox_bundle.inner.clone()).await;
    assert!(result.inner.is_ok(), "Executor failed: {:?}", result.inner);
    cleanup_handle.clean().await.expect("cleanup failed");

    // Callback triggered with TimeoutError — tx was submitted before timeout.
    let callback_calls = test_env.callback_scheduler.recorded_calls();
    assert_eq!(
        callback_calls.len(),
        1,
        "Expected exactly one callback invocation"
    );
    let (callbacks, signature, cb_result) = &callback_calls[0];
    assert!(!callbacks.is_empty(), "Callback list must not be empty");
    assert!(signature.is_none(), "No signature on timeout");
    assert!(
        matches!(cb_result, Err(ActionError::TimeoutError)),
        "Expected TimeoutError, got: {:?}",
        cb_result
    );

    // Despite the callback timeout the commit tx still lands on chain.
    let payer_balance_after = test_env
        .ctx
        .fetch_ephem_account_balance(&payer.pubkey())
        .unwrap();
    assert_eq!(
        payer_balance_after + BASE_ACTION_FEE + CALLBACK_FEE + TRANSFER_AMOUNT,
        payer_balance_before,
        "Payer fees not deducted correctly"
    );
    let dest_balance = test_env
        .ctx
        .fetch_chain_account_balance(&destination.pubkey())
        .unwrap();
    assert_eq!(
        dest_balance, TRANSFER_AMOUNT,
        "Destination did not receive funds"
    );
}

/// Verifies recovery when the validator crashes after the commit tx lands on chain
/// but before the finalize stage is recorded in the outbox.
///
/// Flow:
///   1. AcceptedIntentExecutor runs TwoStage; commit stage completes (tx on chain,
///      outbox updated to TwoStage::Committing).
///   2. set_intent_execution_stage for the Finalizing stage fails → executor errors
///      and finalize tx is never sent.
///   3. On restart, build_stage_intent_executor sees TwoStage::Committing, confirms
///      the commit sig on chain, then executes the finalize stage.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pickup_after_committing() {
    const COUNTER_COUNT: usize = 9;

    let test_env = TestEnv::setup().await;
    let counters = setup_counters(&test_env.ctx, COUNTER_COUNT);
    let counter_pdas: Vec<Pubkey> =
        counters.iter().map(|(_, pda)| *pda).collect();

    let outbox_bundle = schedule_and_accept(&test_env.ctx, |ctx| {
        schedule_commit_and_undelegate_bundle(ctx, &counter_pdas);
    });

    // Fail only when the executor tries to record the Finalizing stage.
    // The commit stage (Committing) is allowed through so the commit tx is sent.
    let mut failing_outbox = test_env.outbox_client();
    failing_outbox.with_fail_finalizing(true);
    let stage_calls = failing_outbox.stage_calls.clone();

    let executor_ctx = test_env
        .executor_ctx_builder()
        .with_outbox_client(failing_outbox.into())
        .build();

    let executor = Box::new(AcceptedIntentExecutor::new(
        executor_ctx,
        DEFAULT_ACTIONS_TIMEOUT,
    ));
    let (result, cleanup_handle) =
        executor.execute(outbox_bundle.inner.clone()).await;
    assert!(
        result.inner.is_err(),
        "Expected failure on set_intent_execution_stage(Finalizing)"
    );
    cleanup_handle.clean().await.expect("cleanup after failure");

    // Commit stage was recorded; outbox is TwoStage::Committing
    let calls = stage_calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "Only the commit stage should be recorded");
    let commit_sig = match &calls[0].1 {
        ExecutionStage::TwoStage(TwoStageProgress::Committing(sig)) => *sig,
        other => panic!("Expected Committing stage, got {:?}", other),
    };
    drop(calls);

    // Re-fetch outbox and confirm it reflects TwoStage::Committing
    let outbox_bundle = test_env
        .outbox_client()
        .outbox_reader()
        .fetch_outbox_intent(outbox_bundle.inner.id)
        .await
        .expect("fetch succeeded")
        .expect("outbox bundle present");
    assert!(
        matches!(
            &outbox_bundle.status,
            OutboxIntentBundleStatus::Executing(ExecutionStage::TwoStage(
                TwoStageProgress::Committing(_)
            ))
        ),
        "Expected TwoStage::Committing in outbox, got {:?}",
        outbox_bundle.status
    );

    // Recovery: build_stage_intent_executor detects the commit sig on chain and
    // runs the finalize stage without re-submitting the commit tx.
    let executor = build_stage_intent_executor(
        test_env.executor_ctx_builder().build(),
        outbox_bundle.status,
        DEFAULT_ACTIONS_TIMEOUT,
    );
    let (result, cleanup_handle) = Box::new(executor)
        .execute(outbox_bundle.inner.clone())
        .await;
    let ExecutionOutput::TwoStage {
        commit_signature,
        finalize_signature: _,
    } = result.inner.expect("recovery execution succeeded")
    else {
        panic!("Expected TwoStage output");
    };
    assert_eq!(
        commit_signature, commit_sig,
        "Commit sig must not change on recovery"
    );
    cleanup_handle
        .clean()
        .await
        .expect("cleanup after recovery");

    // Counters should be finalized (undelegated) on chain
    for (_, pda) in &counters {
        let counter = test_env
            .ctx
            .fetch_chain_account_struct::<FlexiCounter>(*pda)
            .unwrap();
        assert!(counter.count > 0, "counter should be committed on chain");
    }
}

/// Verifies recovery when the validator crashes after both the commit and
/// finalize txs have landed but before the service can act on the result.
///
/// Flow:
///   1. AcceptedIntentExecutor runs TwoStage to completion; outbox is updated to
///      TwoStage::Finalizing with both signatures.
///   2. On restart, build_stage_intent_executor sees TwoStage::Finalizing, confirms
///      the finalize sig on chain, and returns the same signatures without
///      re-submitting anything.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_pickup_after_finalizing() {
    const COUNTER_COUNT: usize = 9;

    let test_env = TestEnv::setup().await;
    let counters = setup_counters(&test_env.ctx, COUNTER_COUNT);
    let counter_pdas: Vec<Pubkey> =
        counters.iter().map(|(_, pda)| *pda).collect();

    let outbox_bundle = schedule_and_accept(&test_env.ctx, |ctx| {
        schedule_commit_and_undelegate_bundle(ctx, &counter_pdas);
    });

    // Run to completion with a normal outbox client
    let executor_ctx = test_env.executor_ctx_builder().build();
    let executor = Box::new(AcceptedIntentExecutor::new(
        executor_ctx,
        DEFAULT_ACTIONS_TIMEOUT,
    ));
    let (result, cleanup_handle) =
        executor.execute(outbox_bundle.inner.clone()).await;
    let ExecutionOutput::TwoStage {
        commit_signature,
        finalize_signature,
    } = result.inner.expect("first execution succeeded")
    else {
        panic!("Expected TwoStage output");
    };
    cleanup_handle
        .clean()
        .await
        .expect("cleanup after first run");

    // Outbox should now show TwoStage::Finalizing
    let outbox_bundle = test_env
        .outbox_client()
        .outbox_reader()
        .fetch_outbox_intent(outbox_bundle.inner.id)
        .await
        .expect("fetch succeeded")
        .expect("outbox bundle present");
    assert!(
        matches!(
            &outbox_bundle.status,
            OutboxIntentBundleStatus::Executing(ExecutionStage::TwoStage(
                TwoStageProgress::Finalizing { .. }
            ))
        ),
        "Expected TwoStage::Finalizing in outbox, got {:?}",
        outbox_bundle.status
    );

    // Recovery: build_stage_intent_executor detects the finalize sig on chain and
    // returns the same signatures without re-executing either stage.
    let executor = build_stage_intent_executor(
        test_env.executor_ctx_builder().build(),
        outbox_bundle.status,
        DEFAULT_ACTIONS_TIMEOUT,
    );
    let (result, cleanup_handle) = Box::new(executor)
        .execute(outbox_bundle.inner.clone())
        .await;
    let ExecutionOutput::TwoStage {
        commit_signature: retried_commit,
        finalize_signature: retried_finalize,
    } = result.inner.expect("recovery execution succeeded")
    else {
        panic!("Expected TwoStage output");
    };
    assert_eq!(
        retried_commit, commit_signature,
        "Commit sig must not change on recovery"
    );
    assert_eq!(
        retried_finalize, finalize_signature,
        "Finalize sig must not change on recovery"
    );
    cleanup_handle
        .clean()
        .await
        .expect("cleanup after recovery");

    // Counters are finalized and undelegated on chain
    for (_, pda) in &counters {
        let counter = test_env
            .ctx
            .fetch_chain_account_struct::<FlexiCounter>(*pda)
            .unwrap();
        assert!(counter.count > 0, "counter should be committed on chain");
    }
}

#[derive(Clone, Default)]
struct RecordingCallbackScheduler {
    calls: Arc<Mutex<Vec<CallbackRecord>>>,
}

impl RecordingCallbackScheduler {
    fn recorded_calls(&self) -> Vec<CallbackRecord> {
        self.calls.lock().unwrap().clone()
    }
}

impl ActionsCallbackScheduler for RecordingCallbackScheduler {
    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        signature: Option<Signature>,
        result: ActionResult,
    ) -> Vec<Result<Signature, CallbackScheduleError>> {
        let count = callbacks.len();
        self.calls
            .lock()
            .unwrap()
            .push((callbacks, signature, result));
        (0..count).map(|_| Ok(Signature::new_unique())).collect()
    }
}

struct TestOutboxReader(Arc<AsyncRpcClient>);

#[async_trait]
impl OutboxIntentBundlesReader for TestOutboxReader {
    type Error = anyhow::Error;

    async fn read(
        &mut self,
        _n: usize,
    ) -> Result<Vec<OutboxIntentBundle>, Self::Error> {
        Ok(vec![])
    }

    async fn fetch_outbox_intent(
        &self,
        intent_id: u64,
    ) -> Result<Option<OutboxIntentBundle>, Self::Error> {
        let pda = outbox_intent_pda(intent_id);
        let mut accounts = self.0.get_multiple_accounts(&[pda]).await?;
        let Some(account) = accounts.pop().flatten() else {
            return Ok(None);
        };
        Ok(Some(OutboxIntentBundle::try_from_bytes(&account.data)?))
    }
}

struct TestOutboxClient {
    ephem_rpc: Arc<AsyncRpcClient>,
    validator_keypair: Keypair,
    pub stage_calls: Arc<Mutex<Vec<(u64, ExecutionStage)>>>,
    pub sent_commits: Arc<Mutex<Vec<(u64, bool)>>>,

    fail_set_execution_stage: bool,
    set_execution_stage_sleep: Option<Duration>,
    fail_committing: bool,
    fail_finalizing: bool,
}

impl TestOutboxClient {
    fn new(ephem_rpc: Arc<AsyncRpcClient>, validator_keypair: Keypair) -> Self {
        Self {
            ephem_rpc,
            validator_keypair,
            stage_calls: Default::default(),
            sent_commits: Default::default(),
            fail_set_execution_stage: false,
            set_execution_stage_sleep: None,
            fail_committing: false,
            fail_finalizing: false,
        }
    }

    fn with_fail_execution_stage(&mut self, value: bool) {
        self.fail_set_execution_stage = value
    }

    fn with_set_execution_stage_sleep(&mut self, value: Duration) {
        self.set_execution_stage_sleep = Some(value);
    }

    fn with_fail_committing(&mut self, value: bool) {
        self.fail_committing = value;
    }

    fn with_fail_finalizing(&mut self, value: bool) {
        self.fail_finalizing = value;
    }
}

#[async_trait]
impl OutboxClient for TestOutboxClient {
    type Error = InternalOutboxClientError;
    type OutboxReader = TestOutboxReader;

    async fn accept_scheduled_intents(
        &self,
    ) -> Result<
        Vec<ScheduledIntentBundle>,
        (Vec<ScheduledIntentBundle>, Self::Error),
    > {
        Ok(vec![])
    }

    async fn set_intent_execution_stage(
        &self,
        intent_id: u64,
        stage: ExecutionStage,
    ) -> Result<(), Self::Error> {
        if let Some(sleep_duration) = self.set_execution_stage_sleep {
            tokio::time::sleep(sleep_duration).await;
        };

        let should_fail = self.fail_set_execution_stage
            || matches!(
                &stage,
                ExecutionStage::TwoStage(TwoStageProgress::Committing(_))
                    if self.fail_committing
            )
            || matches!(
                &stage,
                ExecutionStage::TwoStage(TwoStageProgress::Finalizing { .. })
                    if self.fail_finalizing
            );

        if should_fail {
            return Err(Self::Error::RpcClientError(
                client_error::ErrorKind::Custom(
                    "set_intent_execution_stage failed".to_string(),
                )
                .into(),
            ));
        }

        self.stage_calls
            .lock()
            .unwrap()
            .push((intent_id, stage.clone()));
        let blockhash = self
            .ephem_rpc
            .get_latest_blockhash()
            .await
            .map_err(InternalOutboxClientError::RpcClientError)?;
        let ix = Instruction::new_with_bincode(
            magicblock_magic_program_api::id(),
            &MagicBlockInstruction::SetIntentExecutionStage {
                intent_id,
                stage,
            },
            vec![
                AccountMeta::new_readonly(
                    self.validator_keypair.pubkey(),
                    true,
                ),
                AccountMeta::new(outbox_intent_pda(intent_id), false),
            ],
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.validator_keypair.pubkey()),
            &[&self.validator_keypair],
            blockhash,
        );
        self.ephem_rpc
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(InternalOutboxClientError::RpcClientError)?;
        Ok(())
    }

    async fn notify_commit_sent(
        &self,
        meta: ScheduledBaseIntentMeta,
        result: &IntentExecutorResult<ExecutionOutput>,
        _execution_report: &IntentExecutionReport,
    ) -> Result<(), Self::Error> {
        let succeeded = result.is_ok();
        self.ephem_rpc
            .send_and_confirm_transaction(&meta.intent_sent_transaction)
            .await
            .map_err(InternalOutboxClientError::RpcClientError)?;
        self.sent_commits.lock().unwrap().push((meta.id, succeeded));
        Ok(())
    }

    fn outbox_reader(&self) -> Self::OutboxReader {
        TestOutboxReader(self.ephem_rpc.clone())
    }
}

struct SleepyRpcSender {
    inner: HttpSender,
    sleep_duration: Duration,
}

#[async_trait]
impl RpcSender for SleepyRpcSender {
    async fn send(
        &self,
        request: RpcRequest,
        params: serde_json::Value,
    ) -> solana_rpc_client_api::client_error::Result<serde_json::Value> {
        let result = self.inner.send(request, params).await;
        tokio::time::sleep(self.sleep_duration).await;
        result
    }

    fn get_transport_stats(&self) -> RpcTransportStats {
        self.inner.get_transport_stats()
    }

    fn url(&self) -> String {
        self.inner.url()
    }
}

fn ensure_validator_authority() -> Keypair {
    static ONCE: Once = Once::new();
    let kp = Keypair::try_from(&DLP_TEST_AUTHORITY_BYTES[..]).unwrap();
    ONCE.call_once(|| {
        init_validator_authority(kp.insecure_clone());
    });
    kp
}

fn read_next_intent_id(ctx: &IntegrationTestContext) -> u64 {
    let data = ctx.fetch_ephem_account_data(MAGIC_CONTEXT_PUBKEY).unwrap();
    MagicContext::intent_id(&data)
}

pub fn schedule_commit_instruction(
    payer: &Pubkey,
    pdas: Vec<Pubkey>,
) -> Instruction {
    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
    ];
    for pubkey in &pdas {
        account_metas.push(AccountMeta::new_readonly(*pubkey, false));
    }
    Instruction::new_with_bincode(
        magicblock_magic_program_api::id(),
        &MagicBlockInstruction::ScheduleCommit,
        account_metas,
    )
}

/// Schedules a CommitAndUndelegate bundle via ScheduleIntentBundle signed by the
/// validator authority.  Using CommitAndUndelegate (vs CommitFinalize) generates
/// separate commit_tasks + finalize_tasks so build_execution_strategy selects
/// TwoStage execution.  With 5 accounts the combined SingleStage tx needs ALTs
/// while each individual stage fits without them, guaranteeing TwoStage.
fn schedule_commit_and_undelegate_bundle_instruction(
    payer: &Pubkey,
    counter_pdas: &[Pubkey],
) -> Instruction {
    // Indices are 1-based into the instruction account list:
    // [0]=payer, [1]=magic_context, [2..]=counter PDAs
    let indices: Vec<u8> = (2u8..2 + counter_pdas.len() as u8).collect();
    let args = MagicIntentBundleArgs {
        commit: None,
        commit_and_undelegate: Some(CommitAndUndelegateArgs {
            commit_type: CommitTypeArgs::Standalone(indices),
            undelegate_type: UndelegateTypeArgs::Standalone,
        }),
        commit_finalize: None,
        commit_finalize_and_undelegate: None,
        standalone_actions: vec![],
    };
    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
    ];
    account_metas
        .extend(counter_pdas.iter().map(|pk| AccountMeta::new(*pk, false)));
    Instruction::new_with_bincode(
        magicblock_magic_program_api::id(),
        &MagicBlockInstruction::ScheduleIntentBundle(args),
        account_metas,
    )
}

fn schedule_commit_and_undelegate_bundle(
    ctx: &IntegrationTestContext,
    counter_pdas: &[Pubkey],
) {
    let validator_keypair = ensure_validator_authority();
    let ix = schedule_commit_and_undelegate_bundle_instruction(
        &validator_keypair.pubkey(),
        counter_pdas,
    );
    let mut tx =
        Transaction::new_with_payer(&[ix], Some(&validator_keypair.pubkey()));
    let sig = ctx
        .send_transaction_ephem(&mut tx, &[&validator_keypair])
        .unwrap();
    println!("schedule_commit_and_undelegate_bundle sig: {}", sig);
}

fn setup_counters(
    ctx: &IntegrationTestContext,
    n: usize,
) -> Vec<(Keypair, Pubkey)> {
    (0..n)
        .map(|i| {
            let payer = setup_payer(ctx);
            init_counter(ctx, &payer);
            delegate_counter(ctx, &payer);
            add_to_counter(ctx, &payer, (i + 1) as u8);
            let pda = FlexiCounter::pda(&payer.pubkey()).0;
            (payer, pda)
        })
        .collect()
}
