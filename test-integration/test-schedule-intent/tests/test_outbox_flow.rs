mod common;
use std::sync::{Arc, Mutex, Once};

use async_trait::async_trait;
use common::*;
use integration_test_tools::{
    loaded_accounts::DLP_TEST_AUTHORITY_BYTES, IntegrationTestContext,
};
use magicblock_committor_service::{
    intent_executor::{
        accepted_intent_executor::AcceptedIntentExecutor,
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
        IntentExecutor, IntentExecutorCtx,
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
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox_intent_bundles::OutboxIntentBundle,
    validator::init_validator_authority, MagicContext, SentCommit,
};
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use program_flexi_counter::{
    instruction::create_intent_bundle_ix, state::FlexiCounter,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient as AsyncRpcClient;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

// ---------------------------------------------------------------------------
// NoopCallbackScheduler
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// TestOutboxClient
// ---------------------------------------------------------------------------

struct TestOutboxReader;

#[async_trait]
impl OutboxIntentBundlesReader for TestOutboxReader {
    type Error = std::convert::Infallible;

    async fn read(
        &mut self,
        _n: usize,
    ) -> Result<Vec<OutboxIntentBundle>, Self::Error> {
        Ok(vec![])
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
        TestOutboxReader
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// Sends a single ER transaction: [schedule_commit_ix, accept_ix].
/// Both instructions land atomically — MagicContext is drained within the
/// same tx so CommittorService never sees the intent.
/// Returns the OutboxIntentBundle in Accepted stage.
fn schedule_and_accept(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
) -> OutboxIntentBundle {
    ctx.wait_for_next_slot_ephem().unwrap();

    let validator_keypair = ensure_validator_authority();
    let intent_id = read_next_intent_id(ctx);

    let destination = Keypair::new();
    let schedule_ix = create_intent_bundle_ix(
        vec![payer.pubkey()],
        vec![],
        destination.pubkey(),
        vec![],
        100_000,
    );
    let accept_ix = Instruction::new_with_bincode(
        magicblock_magic_program_api::id(),
        &MagicBlockInstruction::AcceptScheduleCommits,
        vec![
            AccountMeta::new_readonly(validator_keypair.pubkey(), true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            AccountMeta::new(outbox_intent_pda(intent_id), false),
        ],
    );

    let mut tx = Transaction::new_with_payer(
        &[schedule_ix, accept_ix],
        Some(&payer.pubkey()),
    );
    let (_, confirmed) = ctx
        .send_and_confirm_transaction_ephem(
            &mut tx,
            &[payer, &validator_keypair],
        )
        .unwrap();
    assert!(confirmed, "schedule_and_accept tx not confirmed");

    let pda = outbox_intent_pda(intent_id);
    let data = ctx.fetch_ephem_account_data(pda).unwrap();
    OutboxIntentBundle::try_from_bytes(&data).unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// AcceptedIntentExecutor drives the full commit flow using TestOutboxClient.
/// Verifies: executor succeeds, set_intent_execution_stage is called,
/// and the committed value lands on the base chain.
#[tokio::test]
#[ignore]
async fn test_accepted_executor_outbox_flow() {
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = setup_payer(&ctx);

    init_counter(&ctx, &payer);
    delegate_counter(&ctx, &payer);
    add_to_counter(&ctx, &payer, 42);

    let outbox_bundle = schedule_and_accept(&ctx, &payer);
    let intent_id = outbox_bundle.inner.id;

    let validator_keypair = ensure_validator_authority();
    let chain_rpc = Arc::new(ctx.try_chain_client_async().unwrap());
    let ephem_rpc = Arc::new(ctx.try_ephem_client_async().unwrap());
    let chain_mb_rpc = MagicblockRpcClient::new(chain_rpc);
    let ephem_mb_rpc = MagicblockRpcClient::new(ephem_rpc.clone());

    let outbox_client = Arc::new(TestOutboxClient::new(
        ephem_rpc,
        validator_keypair.insecure_clone(),
    ));
    let stage_calls = outbox_client.stage_calls.clone();

    let gc_config = GarbageCollectorConfig::default();
    let table_mania = TableMania::new(
        chain_mb_rpc.clone(),
        &validator_keypair,
        Some(gc_config),
    );
    let compute_budget = ComputeBudgetConfig::new(1_000_000);
    let task_info_fetcher = Arc::new(CacheTaskInfoFetcher::new(
        RpcTaskInfoFetcher::new(ephem_mb_rpc),
    ));
    let transaction_preparator = TransactionPreparatorImpl::new(
        chain_mb_rpc.clone(),
        table_mania,
        compute_budget,
    );

    let executor = AcceptedIntentExecutor::new(IntentExecutorCtx {
        intent_client: IntentExecutionClient::new(chain_mb_rpc),
        transaction_preparator,
        task_info_fetcher,
        outbox_client,
        actions_callback_executor: NoopCallbackScheduler,
        actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
    });

    let (result, cleanup_handle) =
        Box::new(executor).execute(outbox_bundle.inner).await;
    let _ = cleanup_handle.clean().await;

    assert!(result.inner.is_ok(), "Executor failed: {:?}", result.inner);
    let calls = stage_calls.lock().unwrap();
    assert!(
        !calls.is_empty(),
        "Expected at least one set_intent_execution_stage call"
    );
    assert!(
        calls.iter().all(|(id, _)| *id == intent_id),
        "Stage calls contain unexpected intent ids"
    );

    let counter_pda = FlexiCounter::pda(&payer.pubkey()).0;
    let counter = ctx
        .fetch_chain_account_struct::<FlexiCounter>(counter_pda)
        .unwrap();
    assert_eq!(counter.count, 42);
}
