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
    outbox::{ExecutionStage, TwoStageProgress},
    outbox_intent_bundles::OutboxIntentBundle,
};
use rpc_client::{MagicBlockRpcClientError, MagicblockRpcClient};
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_rpc_client::{mock_sender::MocksMap, nonblocking::rpc_client::RpcClient};
use solana_rpc_client_api::{
    client_error::{Error as RpcClientError, ErrorKind as RpcClientErrorKind},
    request::{RpcError, RpcRequest, RpcResponseErrorData},
};
use solana_signature::Signature;
use solana_signer::Signer;

use crate::{
    execution::{
        ExecutionContext,
        preparation::{
            TransactionPreparator,
            cleanup_resources::CleanupResources,
            delivery_preparator::{BufferExecutionError, DeliveryPreparatorResult},
            error::{PreparatorResult, TransactionPreparatorError},
        },
    },
    shared::{
        account_state::{CachedTaskInfoProvider, RpcTaskInfoProvider},
        outbox::OutboxClient,
        tasks::{
            Task, buffer_task::BufferCleanup, task_strategist::TransactionStrategy,
            utils::TransactionUtils,
        },
    },
};

pub(super) const ACCOUNT: Pubkey = Pubkey::new_from_array([7; 32]);
pub(super) const RESERVED_ALT_KEY: Pubkey = Pubkey::new_from_array([9; 32]);

pub(super) fn send_transaction_error(message: &str) -> MagicBlockRpcClientError {
    MagicBlockRpcClientError::SendTransaction(Box::new(RpcClientError {
        request: Some(RpcRequest::SendTransaction),
        kind: Box::new(RpcClientErrorKind::RpcError(RpcError::RpcResponseError {
            code: -32602,
            message: message.to_string(),
            data: RpcResponseErrorData::Empty,
        })),
    }))
}

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
    async fn set_intent_execution_stage(
        &self,
        _: u64,
        stage: ExecutionStage,
        nonces: Vec<(Pubkey, u64)>,
    ) -> Result<(), crate::shared::outbox::outbox_client::InternalOutboxClientError> {
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

    async fn notify_commit_sent(
        &self,
        sent_commit: SentCommit,
        _: crate::shared::outbox::IntentSentTransaction,
    ) -> Result<(), crate::shared::outbox::outbox_client::InternalOutboxClientError> {
        self.reports.lock().unwrap().push(sent_commit);
        if self.fail_reporting {
            return Err(
                crate::shared::outbox::outbox_client::InternalOutboxClientError::TransactionError(
                    solana_transaction_error::TransactionError::AccountNotFound,
                ),
            );
        }
        Ok(())
    }
    async fn fetch_outbox_intent(
        &self,
        _: u64,
    ) -> Result<
        Option<OutboxIntentBundle>,
        crate::shared::outbox::outbox_client::InternalOutboxClientError,
    > {
        Ok(None)
    }
}

pub(super) enum Preparation {
    Ready,
    Fail,
    Wait,
}

#[derive(Clone, Default)]
pub(super) struct RecordingPreparator {
    pub steps: Arc<Mutex<VecDeque<Preparation>>>,
    pub prepared: Arc<Mutex<Vec<Vec<Task>>>>,
    pub uniqueness_nonces: Arc<Mutex<Vec<Option<u64>>>>,
    pub cleaned: Arc<Mutex<Vec<(CleanupResources, bool)>>>,
}

#[async_trait]
impl TransactionPreparator for RecordingPreparator {
    async fn prepare_for_strategy(
        &self,
        authority: &Keypair,
        strategy: &TransactionStrategy,
        cleanup: &mut CleanupResources,
    ) -> PreparatorResult<VersionedMessage> {
        self.prepared.lock().unwrap().push(strategy.tasks.clone());
        self.uniqueness_nonces.lock().unwrap().push(strategy.uniqueness_nonce);
        // Represent a reservation made before preparation succeeds or fails.
        cleanup.alt_reservation.insert(RESERVED_ALT_KEY);
        cleanup.uniqueness_nonce = strategy.uniqueness_nonce;
        cleanup.buffers = strategy
            .tasks
            .iter()
            .filter_map(|task| match task {
                Task::CommitFinalize(task) => BufferCleanup::from_commit_finalize(task),
                _ => None,
            })
            .collect();
        let step = self.steps.lock().unwrap().pop_front().unwrap_or(Preparation::Ready);
        match step {
            Preparation::Fail => Err(TransactionPreparatorError::FailedToFit),
            Preparation::Wait => futures_util::future::pending().await,
            Preparation::Ready => {
                let mut instructions = TransactionUtils::budget_instructions(
                    TransactionUtils::tasks_compute_units(
                        &strategy.tasks,
                        strategy.uniqueness_nonce,
                    ),
                    0,
                    TransactionUtils::tasks_accounts_size_budget(&strategy.tasks),
                )
                .to_vec();
                instructions.extend(TransactionUtils::tasks_instructions(
                    &authority.pubkey(),
                    &strategy.tasks,
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

    async fn cleanup_resources(
        &self,
        _: &Keypair,
        resources: &CleanupResources,
        close_buffers: bool,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        self.cleaned.lock().unwrap().push((resources.clone(), close_buffers));
        Ok(())
    }
}

type CallbackResults = Vec<(Option<Signature>, ActionResult, Pubkey)>;

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
            .map(|callback| {
                self.0.lock().unwrap().push((
                    signature,
                    result.clone(),
                    callback.destination_program,
                ));
                Ok(Signature::new_unique())
            })
            .collect()
    }
}

pub(super) struct Executor {
    pub context: ExecutionContext,
    pub transaction_preparator: RecordingPreparator,
    pub outbox_client: Arc<RecordingOutbox>,
    pub actions_callback_executor: Callbacks,
}

impl std::ops::Deref for Executor {
    type Target = ExecutionContext;
    fn deref(&self) -> &ExecutionContext {
        &self.context
    }
}
impl std::ops::DerefMut for Executor {
    fn deref_mut(&mut self) -> &mut ExecutionContext {
        &mut self.context
    }
}

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
    let transaction_preparator = RecordingPreparator {
        steps: Arc::new(Mutex::new(steps.into())),
        ..Default::default()
    };
    let outbox_client = Arc::new(RecordingOutbox {
        expected_commit_nonces: vec![(ACCOUNT, 2)],
        ..Default::default()
    });
    let actions_callback_executor = Callbacks::default();
    let context = ExecutionContext {
        authority: Keypair::new(),
        intent_client: crate::execution::client::IntentExecutionClient::new(rpc.clone(), 0),
        transaction_preparator: Arc::new(transaction_preparator.clone()),
        task_info_provider: Arc::new(CachedTaskInfoProvider::new(RpcTaskInfoProvider::new(rpc))),
        outbox_client: outbox_client.clone(),
        actions_callback_executor: Arc::new(actions_callback_executor.clone()),
        actions_timeout: std::time::Duration::from_secs(60),
        maintenance: tokio_util::task::TaskTracker::new(),
        cancel: tokio_util::sync::CancellationToken::new(),
    };
    Executor {
        context,
        transaction_preparator,
        outbox_client,
        actions_callback_executor,
    }
}
