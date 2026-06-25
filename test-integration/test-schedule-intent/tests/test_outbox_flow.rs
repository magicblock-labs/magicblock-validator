mod common;
use std::{
    sync::{Arc, Mutex, Once},
    time::Duration,
};

use anyhow::anyhow;
use async_trait::async_trait;
use common::*;
use ephemeral_rollups_sdk::{compat, ephem::MagicIntentBundleBuilder};
use integration_test_tools::{
    loaded_accounts::DLP_TEST_AUTHORITY_BYTES, IntegrationTestContext,
};
use magicblock_committor_service::{
    intent_executor::{
        accepted_intent_executor::AcceptedIntentExecutor,
        build_stage_intent_executor,
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
        ExecutionOutput, IntentExecutor, IntentExecutorCtx,
    },
    outbox_client::{InternalOutboxClientError, OutboxClient},
    service::outbox_intent_bundles_reader::OutboxIntentBundlesReader,
    transaction_preparator::TransactionPreparatorImpl,
    ComputeBudgetConfig, DEFAULT_ACTIONS_TIMEOUT,
};
use magicblock_core::{
    intent::{outbox::outbox_intent_pda, BaseActionCallback},
    traits::{ActionResult, ActionsCallbackScheduler, CallbackScheduleError},
};
use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, outbox::ExecutionStage,
    MAGIC_CONTEXT_PUBKEY,
};
use magicblock_program::{
    instruction_utils::InstructionUtils,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent_bundles::{OutboxIntentBundle, OutboxIntentBundleStatus},
    validator::init_validator_authority,
    MagicContext, SentCommit,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use program_flexi_counter::{
    instruction::{
        create_intent_bundle_commit_and_finalize_ix, create_intent_bundle_ix,
    },
    state::FlexiCounter,
};
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient as AsyncRpcClient,
    rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::client_error;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

type TestIntentExecutorCtx = IntentExecutorCtx<
    TransactionPreparatorImpl,
    RpcTaskInfoFetcher,
    NoopCallbackScheduler,
    TestOutboxClient,
>;

struct TestEnv {
    ctx: IntegrationTestContext,
    validator_authority: Keypair,
    chain_mb_client: MagicblockRpcClient,
    intent_client: IntentExecutionClient,
    outbox_client: Arc<TestOutboxClient>,
    table_mania: TableMania,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<RpcTaskInfoFetcher>>,
    stage_calls: Arc<Mutex<Vec<(u64, ExecutionStage)>>>,
}

impl TestEnv {
    async fn setup() -> Self {
        let validator_authority = ensure_validator_authority();
        let ctx = IntegrationTestContext::try_new().unwrap();
        let chain_rpc = Arc::new(ctx.try_chain_client_async().unwrap());
        let ephem_rpc = Arc::new(ctx.try_ephem_client_async().unwrap());
        let chain_mb_rpc = MagicblockRpcClient::new(chain_rpc);
        let ephem_mb_rpc = MagicblockRpcClient::new(ephem_rpc.clone());

        let intent_client = IntentExecutionClient::new(chain_mb_rpc.clone());
        let outbox_client = Arc::new(TestOutboxClient::new(
            ephem_rpc,
            validator_authority.insecure_clone(),
        ));
        let stage_calls = outbox_client.stage_calls.clone();

        let gc_config = GarbageCollectorConfig::default();
        let table_mania = TableMania::new(
            chain_mb_rpc.clone(),
            &validator_authority,
            Some(gc_config),
        );
        let compute_budget = ComputeBudgetConfig::new(1_000_000);
        let task_info_fetcher = Arc::new(CacheTaskInfoFetcher::new(
            RpcTaskInfoFetcher::new(chain_mb_rpc.clone()),
        ));

        Self {
            ctx,
            validator_authority,
            chain_mb_client: chain_mb_rpc,
            intent_client,
            outbox_client,
            table_mania,
            task_info_fetcher,

            stage_calls,
        }
    }

    fn executor_ctx(&self, actions_timeout: Duration) -> TestIntentExecutorCtx {
        TestIntentExecutorCtx {
            intent_client: self.intent_client.clone(),
            transaction_preparator: self.transaction_preparator(),
            task_info_fetcher: self.task_info_fetcher.clone(),
            outbox_client: self.outbox_client.clone(),
            actions_callback_executor: NoopCallbackScheduler,
            actions_timeout,
        }
    }

    fn transaction_preparator(&self) -> TransactionPreparatorImpl {
        let compute_budget = ComputeBudgetConfig::new(1_000_000);
        TransactionPreparatorImpl::new(
            self.chain_mb_client.clone(),
            self.table_mania.clone(),
            compute_budget,
        )
    }
}

/// Schedyles commit of counters directly via magic-program using validator authority
fn schedule(ctx: &IntegrationTestContext, counters: &[Pubkey]) {
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
    println!("schedule sig: {}", sig);
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
    counters: &[Pubkey],
) -> u64 {
    const MAX_ATTEMPTS: u8 = 5;

    let mut attempt = 0;
    loop {
        let intent_id = read_next_intent_id(ctx);
        schedule(ctx, counters);
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
    counters: &[Pubkey],
) -> OutboxIntentBundle {
    ctx.wait_for_next_slot_ephem().unwrap();

    let intent_id = steal_schedule_accept_intent(&ctx, counters);
    let pda = outbox_intent_pda(intent_id);
    let data = ctx.fetch_ephem_account_data(pda).unwrap();
    OutboxIntentBundle::try_from_bytes(&data).unwrap()
}

/// AcceptedIntentExecutor drives the full commit flow using TestOutboxClient.
/// Verifies: executor succeeds, set_intent_execution_stage is called,
/// and the committed value lands on the base chain.
#[tokio::test(flavor = "multi_thread")]
async fn test_pickup_executed_intent() {
    let test_env = TestEnv::setup().await;

    let payer = setup_payer(&test_env.ctx);
    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;

    init_counter(&test_env.ctx, &payer);
    delegate_counter(&test_env.ctx, &payer);
    add_to_counter(&test_env.ctx, &payer, 42);
    let outbox_bundle = schedule_and_accept(&test_env.ctx, &[counter_pda]);

    // Execute intent
    let executor_ctx = test_env.executor_ctx(DEFAULT_ACTIONS_TIMEOUT);
    let executor = AcceptedIntentExecutor::new(executor_ctx);
    let (result, cleanup_handle) = Box::new(executor)
        .execute(outbox_bundle.inner.clone())
        .await;
    assert!(result.inner.is_ok(), "Executor failed: {:?}", result.inner);

    cleanup_handle.clean().await.expect("cleanup failed");

    // Simulates shutdown/crash after successful execution
    // Validator could sent tx and then crash
    // On restart validator will extract outbox bundles
    let outbox_bundle = test_env
        .outbox_client
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
        test_env.executor_ctx(DEFAULT_ACTIONS_TIMEOUT),
        outbox_bundle.status,
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

#[derive(Clone, Default)]
struct NoopCallbackScheduler;
impl ActionsCallbackScheduler for NoopCallbackScheduler {
    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        _signature: Option<Signature>,
        _result: ActionResult,
    ) -> Vec<Result<Signature, CallbackScheduleError>> {
        callbacks
            .iter()
            .map(|_| Ok(Signature::new_unique()))
            .collect()
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
}

impl TestOutboxClient {
    fn new(ephem_rpc: Arc<AsyncRpcClient>, validator_keypair: Keypair) -> Self {
        Self {
            ephem_rpc,
            validator_keypair,
            stage_calls: Default::default(),
        }
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
        _sent_tx: Transaction,
        _sent_commit: SentCommit,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn outbox_reader(&self) -> Self::OutboxReader {
        TestOutboxReader(self.ephem_rpc.clone())
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
