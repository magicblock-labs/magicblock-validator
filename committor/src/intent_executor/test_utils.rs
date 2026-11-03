use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use mbv_core::{
    intent::{BaseAction, BaseActionCallback, ProgramArgs},
    traits::{ActionResult, ActionsCallbackScheduler, CallbackScheduleError},
};
use program::{
    SentCommit,
    magic_scheduled_base_intent::ScheduledIntentBundle,
    outbox::{ExecutionStage, TwoStageProgress},
    outbox_intent_bundles::OutboxIntentBundle,
};
use rpc_client::MagicblockRpcClient;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_rpc_client::{mock_sender::MocksMap, nonblocking::rpc_client::RpcClient};
use solana_rpc_client_api::request::RpcRequest;
use solana_signature::Signature;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport, IntentExecutorCtx,
        error::{IntentExecutorError, IntentExecutorResult},
    },
    outbox::{
        OutboxClient, ScheduledBaseIntentMeta,
        outbox_intent_bundles_reader::OutboxIntentBundlesReader, utils::build_sent_commit,
    },
    tasks::{
        BaseTaskImpl,
        task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
        task_strategist::TransactionStrategy,
        utils::TransactionUtils,
    },
    transaction_preparator::{
        TransactionPreparator,
        delivery_preparator::{BufferExecutionError, DeliveryPreparatorResult},
        error::{PreparatorResult, TransactionPreparatorError},
    },
};

pub(super) const ACCOUNT: Pubkey = Pubkey::new_from_array([7; 32]);
pub(super) const RESERVED_ALT_KEY: Pubkey = Pubkey::new_from_array([9; 32]);

pub(super) fn base_action() -> BaseAction {
    let source_program = Pubkey::new_unique();
    BaseAction {
        id: 0,
        destination_program: Pubkey::new_unique(),
        source_program: Some(source_program),
        escrow_authority: Pubkey::new_unique(),
        account_metas_per_program: vec![],
        data_per_program: ProgramArgs { data: vec![], escrow_index: 0 },
        compute_units: 10_000,
        callback: Some(BaseActionCallback {
            destination_program: source_program,
            discriminator: vec![],
            payload: vec![],
            compute_units: 10_000,
            account_metas_per_program: vec![],
        }),
    }
}

#[derive(Clone, Default)]
pub(super) struct RecordingOutbox {
    pub stages: Arc<Mutex<Vec<ExecutionStage>>>,
    pub expected_commit_nonces: Vec<(Pubkey, u64)>,
    pub reports: Arc<Mutex<Vec<SentCommit>>>,
    pub fail_reporting: bool,
}

#[async_trait]
impl OutboxClient for RecordingOutbox {
    type Error = IntentExecutorError;
    type OutboxReader = Self;

    async fn set_intent_execution_stage(
        &self,
        _: u64,
        stage: ExecutionStage,
        nonces: Vec<(Pubkey, u64)>,
    ) -> Result<(), Self::Error> {
        let mut stages = self.stages.lock().unwrap();
        if let Some(previous) = stages.last() {
            previous.clone().apply_stage_transition(stage.clone()).unwrap();
        }
        if matches!(
            stage,
            ExecutionStage::TwoStage(TwoStageProgress::Finalizing { .. })
        ) {
            assert!(nonces.is_empty(), "follow-up must not replay commits");
        } else {
            assert_eq!(nonces, self.expected_commit_nonces);
        }
        stages.push(stage);
        Ok(())
    }

    async fn accept_scheduled_intents(
        &self,
    ) -> Result<Vec<ScheduledIntentBundle>, (Vec<ScheduledIntentBundle>, Self::Error)> {
        unreachable!()
    }
    async fn notify_commit_sent(
        &self,
        meta: ScheduledBaseIntentMeta,
        result: &IntentExecutorResult<ExecutionOutput>,
        report: &IntentExecutionReport,
    ) -> Result<(), Self::Error> {
        self.reports.lock().unwrap().push(build_sent_commit(meta, result, report));
        if self.fail_reporting {
            return Err(
                crate::outbox::outbox_client::InternalOutboxClientError::TransactionError(
                    solana_transaction_error::TransactionError::AccountNotFound,
                )
                .into(),
            );
        }
        Ok(())
    }
    async fn close_intent(&self, _: u64) -> Result<(), Self::Error> {
        unreachable!()
    }
    fn outbox_reader(&self) -> Self {
        self.clone()
    }
}

#[async_trait]
impl OutboxIntentBundlesReader for RecordingOutbox {
    type Error = ();
    async fn read(&mut self, _: usize) -> Result<Vec<OutboxIntentBundle>, Self::Error> {
        unreachable!()
    }
    async fn fetch_outbox_intent(&self, _: u64) -> Result<Option<OutboxIntentBundle>, Self::Error> {
        Ok(None)
    }
}

pub(super) enum Preparation {
    Ready,
    Fail,
    Wait,
}

type CleanupRecords = Vec<(Vec<Pubkey>, bool)>;

#[derive(Clone, Default)]
pub(super) struct RecordingPreparator {
    pub steps: Arc<Mutex<VecDeque<Preparation>>>,
    pub prepared: Arc<Mutex<Vec<Vec<BaseTaskImpl>>>>,
    pub uniqueness_nonces: Arc<Mutex<Vec<Option<u64>>>>,
    pub cleaned: Arc<Mutex<CleanupRecords>>,
}

#[async_trait]
impl TransactionPreparator for RecordingPreparator {
    async fn prepare_for_strategy(
        &self,
        authority: &Keypair,
        strategy: &mut TransactionStrategy,
    ) -> PreparatorResult<VersionedMessage> {
        self.prepared.lock().unwrap().push(strategy.optimized_tasks.clone());
        self.uniqueness_nonces.lock().unwrap().push(strategy.uniqueness_nonce);
        // Represent a reservation made before preparation succeeds or fails.
        strategy.lookup_tables_keys = vec![RESERVED_ALT_KEY];
        let step = self.steps.lock().unwrap().pop_front().unwrap_or(Preparation::Ready);
        match step {
            Preparation::Fail => Err(TransactionPreparatorError::FailedToFitError),
            Preparation::Wait => futures_util::future::pending().await,
            Preparation::Ready => {
                let mut instructions = TransactionUtils::budget_instructions(
                    TransactionUtils::tasks_compute_units(
                        &strategy.optimized_tasks,
                        strategy.uniqueness_nonce,
                    ),
                    0,
                    TransactionUtils::tasks_accounts_size_budget(&strategy.optimized_tasks),
                )
                .to_vec();
                instructions.extend(TransactionUtils::tasks_instructions(
                    &authority.pubkey(),
                    &strategy.optimized_tasks,
                ));
                // The RPC mock decodes legacy messages. Keep the same budget
                // instruction offset used by production versioned messages.
                Ok(VersionedMessage::Legacy(Message::new(
                    &instructions,
                    Some(&authority.pubkey()),
                )))
            }
        }
    }

    async fn cleanup_for_strategy(
        &self,
        _: &Keypair,
        strategy: &TransactionStrategy,
        close_buffers: bool,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        self.cleaned
            .lock()
            .unwrap()
            .push((strategy.lookup_tables_keys.clone(), close_buffers));
        Ok(())
    }
}

type CallbackResults = Vec<(Option<Signature>, ActionResult)>;

#[derive(Clone, Default)]
pub(super) struct Callbacks(pub Arc<Mutex<CallbackResults>>);

impl ActionsCallbackScheduler for Callbacks {
    fn schedule(
        &self,
        callbacks: Vec<BaseActionCallback>,
        signature: Option<Signature>,
        result: ActionResult,
    ) -> Vec<Result<Signature, CallbackScheduleError>> {
        callbacks
            .into_iter()
            .map(|_| {
                self.0.lock().unwrap().push((signature, result.clone()));
                Ok(Signature::new_unique())
            })
            .collect()
    }
}

pub(super) type Executor =
    IntentExecutorCtx<RecordingPreparator, RpcTaskInfoFetcher, Callbacks, RecordingOutbox>;

pub(super) fn mock_response(
    mocks: &mut MocksMap,
    request: RpcRequest,
    value: impl std::fmt::Display,
) {
    mocks.insert(
        request,
        format!(r#"{{"context":{{"slot":1}},"value":{value}}}"#).parse().unwrap(),
    );
}

pub(super) fn executor(steps: Vec<Preparation>, error: Option<&str>) -> Executor {
    let mut mocks = MocksMap::default();
    if let Some(error) = error {
        mock_response(
            &mut mocks,
            RpcRequest::GetSignatureStatuses,
            format!(
                r#"[{{"slot":1,"confirmations":null,"err":{error},"status":{{"Err":{error}}},"confirmationStatus":"finalized"}}]"#
            ),
        );
    }
    let rpc = MagicblockRpcClient::from(RpcClient::new_mock_with_mocks_map("succeeds", mocks));
    program::validator::generate_validator_authority_if_needed();
    IntentExecutorCtx {
        authority: Keypair::new(),
        intent_client: crate::intent_executor::intent_execution_client::IntentExecutionClient::new(
            rpc.clone(),
            0,
        ),
        transaction_preparator: RecordingPreparator {
            steps: Arc::new(Mutex::new(steps.into())),
            ..Default::default()
        },
        task_info_fetcher: Arc::new(CacheTaskInfoFetcher::new(RpcTaskInfoFetcher::new(rpc))),
        actions_callback_executor: Callbacks::default(),
        outbox_client: Arc::new(RecordingOutbox {
            expected_commit_nonces: vec![(ACCOUNT, 2)],
            ..Default::default()
        }),
    }
}
