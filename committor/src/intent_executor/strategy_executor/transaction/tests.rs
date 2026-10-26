use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use mbv_core::{
    intent::{BaseAction, BaseActionCallback, ProgramArgs, types::CommittedAccount},
    traits::{ActionResult, CallbackScheduleError},
};
use program::{
    magic_scheduled_base_intent::ScheduledIntentBundle, outbox_intent_bundles::OutboxIntentBundle,
};
use rpc_client::MagicblockRpcClient;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_rpc_client::{mock_sender::MocksMap, nonblocking::rpc_client::RpcClient};
use solana_rpc_client_api::request::RpcRequest;

use super::*;
use crate::{
    outbox::{ScheduledBaseIntentMeta, outbox_intent_bundles_reader::OutboxIntentBundlesReader},
    tasks::task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
    tasks::{
        UndelegateTask,
        utils::{TransactionUtils, create_action_tasks, create_commit_finalize_task},
    },
    transaction_preparator::{
        delivery_preparator::{BufferExecutionError, DeliveryPreparatorResult},
        error::{PreparatorResult, TransactionPreparatorError},
    },
};

const ACCOUNT: Pubkey = Pubkey::new_from_array([7; 32]);
const RESERVED_ALT_KEY: Pubkey = Pubkey::new_from_array([9; 32]);

#[derive(Clone, Default)]
struct RecordingOutbox(Arc<Mutex<Vec<ExecutionStage>>>);

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
        let mut stages = self.0.lock().unwrap();
        if let Some(previous) = stages.last() {
            previous.clone().apply_stage_transition(stage.clone()).unwrap();
        }
        if matches!(
            stage,
            ExecutionStage::TwoStage(TwoStageProgress::Finalizing { .. })
        ) {
            assert!(nonces.is_empty(), "follow-up must not replay commits");
        } else {
            assert_eq!(nonces, vec![(ACCOUNT, 2)]);
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
        _: ScheduledBaseIntentMeta,
        _: &IntentExecutorResult<ExecutionOutput>,
        _: &IntentExecutionReport,
    ) -> Result<(), Self::Error> {
        unreachable!()
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

enum Preparation {
    Ready,
    Fail,
    Wait,
}

#[derive(Clone, Default)]
struct RecordingPreparator {
    steps: Arc<Mutex<VecDeque<Preparation>>>,
    prepared: Arc<Mutex<Vec<Vec<BaseTaskImpl>>>>,
}

#[async_trait]
impl TransactionPreparator for RecordingPreparator {
    async fn prepare_for_strategy(
        &self,
        authority: &Keypair,
        strategy: &mut TransactionStrategy,
    ) -> PreparatorResult<VersionedMessage> {
        self.prepared.lock().unwrap().push(strategy.optimized_tasks.clone());
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
        _: &TransactionStrategy,
        _: bool,
    ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
        Ok(())
    }
}

type CallbackResults = Vec<(Option<Signature>, ActionResult)>;

#[derive(Clone, Default)]
struct Callbacks(Arc<Mutex<CallbackResults>>);

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

type Executor =
    IntentExecutorCtx<RecordingPreparator, RpcTaskInfoFetcher, Callbacks, RecordingOutbox>;

fn mock_response(mocks: &mut MocksMap, request: RpcRequest, value: impl std::fmt::Display) {
    mocks.insert(
        request,
        format!(r#"{{"context":{{"slot":1}},"value":{value}}}"#).parse().unwrap(),
    );
}

fn executor(steps: Vec<Preparation>, error: Option<&str>) -> Executor {
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
        outbox_client: Arc::new(RecordingOutbox::default()),
    }
}

fn commit() -> BaseTaskImpl {
    create_commit_finalize_task(
        2,
        true,
        CommittedAccount {
            pubkey: ACCOUNT,
            account: Account::default(),
            remote_slot: 0,
        },
        None,
    )
    .into()
}

fn undelegate() -> BaseTaskImpl {
    BaseTaskImpl::Undelegate(UndelegateTask {
        delegated_account: ACCOUNT,
        owner_program: Pubkey::new_unique(),
        rent_reimbursement: Pubkey::new_unique(),
        include_undelegation_request: false,
    })
}

fn action() -> BaseTaskImpl {
    let source_program = Pubkey::new_unique();
    create_action_tasks(&[BaseAction {
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
    }])
    .next()
    .unwrap()
}

fn strategy(tasks: Vec<BaseTaskImpl>) -> TransactionStrategy {
    TransactionStrategy {
        optimized_tasks: tasks,
        ..Default::default()
    }
}

fn split(follow_up: Vec<BaseTaskImpl>) -> StrategyExecutionMode {
    StrategyExecutionMode::TwoStage(TwoStageExecutionMode {
        commit_stage: strategy(vec![commit()]),
        finalize_stage: strategy(follow_up),
    })
}

#[tokio::test(start_paused = true)]
async fn timeout_preserves_actions_and_resolves_pending_transactions() {
    for phase in ["combined", "commit", "follow_up"] {
        for has_pending_transaction in [false, true] {
            let steps = if has_pending_transaction { vec![] } else { vec![Preparation::Wait] };
            let mut ctx = executor(steps, None);
            let pending = PendingTransaction {
                signature: Signature::new_unique(),
                blockhash: solana_hash::Hash::default(),
            };
            if has_pending_transaction {
                // The signed transaction may still land. Its first status is unknown;
                // it confirms after the callback deadline while resolving the same signature.
                let mut mocks = MocksMap::default();
                for _ in 0..2 {
                    mock_response(&mut mocks, RpcRequest::IsBlockhashValid, true);
                }
                mock_response(&mut mocks, RpcRequest::GetSignatureStatuses, "[null]");
                let rpc = MagicblockRpcClient::from(RpcClient::new_mock_with_mocks_map(
                    "succeeds", mocks,
                ));
                ctx.intent_client =
                    crate::intent_executor::intent_execution_client::IntentExecutionClient::new(
                        rpc, 0,
                    );
            }
            let mut report = IntentExecutionReport::default();
            let earlier_commit = Signature::new_unique();
            let mut runner = match phase {
                "combined" => TransactionExecutor::new(
                    &ctx,
                    &mut report,
                    42,
                    &[ACCOUNT],
                    StrategyExecutionMode::SingleStage(strategy(vec![commit(), action()])),
                ),
                "commit" => TransactionExecutor::new(
                    &ctx,
                    &mut report,
                    42,
                    &[ACCOUNT],
                    split(vec![undelegate(), action()]),
                ),
                _ => TransactionExecutor::resume_follow_up(
                    &ctx,
                    &mut report,
                    42,
                    earlier_commit,
                    strategy(vec![undelegate(), action()]),
                ),
            };
            if has_pending_transaction {
                runner.pending_transaction = Some(pending);
            }
            let deadline = tokio::time::Instant::now() + Duration::from_millis(10);
            let result = runner
                .execute(|| deadline.checked_duration_since(tokio::time::Instant::now()))
                .await
                .unwrap();
            let callbacks = ctx.actions_callback_executor.0.lock().unwrap();
            assert!(
                matches!(
                    callbacks.as_slice(),
                    [(None, Err(ActionError::TimeoutError))]
                ),
                "{phase}, pending={has_pending_transaction}"
            );
            let prepared = ctx.transaction_preparator.prepared.lock().unwrap();
            assert_eq!(
                prepared.len(),
                usize::from(phase == "commit") + if has_pending_transaction { 0 } else { 2 }
            );
            assert!(prepared.iter().all(|tasks| !tasks.is_empty()));
            assert!(
                report
                    .junk
                    .iter()
                    .flat_map(|s| &s.optimized_tasks)
                    .any(|t| matches!(t, BaseTaskImpl::BaseAction(_))),
                "timeout must retain actions"
            );
            match result {
                ExecutionOutput::SingleStage(signature) => {
                    assert_eq!(phase, "combined");
                    if has_pending_transaction {
                        assert_eq!(signature, pending.signature);
                    }
                }
                ExecutionOutput::TwoStage {
                    commit_signature,
                    finalize_signature,
                } => {
                    assert_ne!(phase, "combined");
                    if phase == "follow_up" {
                        assert_eq!(commit_signature, earlier_commit);
                        if has_pending_transaction {
                            assert_eq!(finalize_signature, pending.signature);
                        }
                    } else if has_pending_transaction {
                        assert_eq!(commit_signature, pending.signature);
                    }
                    if let Some(ExecutionStage::TwoStage(TwoStageProgress::Finalizing {
                        commit,
                        ..
                    })) = ctx.outbox_client.0.lock().unwrap().last()
                    {
                        assert_eq!(*commit, commit_signature);
                    }
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_keeps_attempts_already_spent_on_current_transaction() {
    let executor = executor(vec![Preparation::Wait], None);
    let mut report = IntentExecutionReport::default();
    let mut runner = TransactionExecutor::new(
        &executor,
        &mut report,
        42,
        &[ACCOUNT],
        StrategyExecutionMode::SingleStage(strategy(vec![commit(), action()])),
    );
    runner.current_attempt = 8;
    runner
        .execute_with_timeout(Some(Duration::from_millis(10)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(runner.current_attempt, 10);
}

#[tokio::test]
async fn execution_failure_surrenders_current_and_pending_resources() {
    let nonce_error = format!(
        r#"{{"InstructionError":[{},{{"Custom":{}}}]}}"#,
        TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT,
        delegation_program_api::error::DlpError::NonceOutOfOrder as u32
    );
    // The RPC mock has no delegation record, so nonce recovery fails after
    // preparing the first transaction and receiving its on-chain nonce error.
    for (steps, error) in [
        (vec![Preparation::Fail], None),
        (vec![Preparation::Ready], Some(nonce_error.as_str())),
        (vec![Preparation::Ready, Preparation::Fail], None),
    ] {
        let expected_preparations = steps.len();
        let executor = executor(steps, error);
        let mut report = IntentExecutionReport::default();
        let result = TransactionExecutor::new(
            &executor,
            &mut report,
            42,
            &[ACCOUNT],
            split(vec![undelegate(), action()]),
        )
        .execute(|| Some(Duration::from_secs(60)))
        .await;
        if error.is_some() {
            assert!(
                matches!(result, Err(IntentExecutorError::TaskBuilderError(_))),
                "{result:?}"
            );
        } else if expected_preparations == 1 {
            assert!(
                matches!(
                    result,
                    Err(IntentExecutorError::FailedCommitPreparationError(_))
                ),
                "{result:?}"
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(IntentExecutorError::FailedFinalizePreparationError(_))
                ),
                "{result:?}"
            );
        }
        let prepared = executor.transaction_preparator.prepared.lock().unwrap();
        assert_eq!(prepared.len(), expected_preparations);
        assert!(matches!(
            prepared[0].as_slice(),
            [BaseTaskImpl::CommitFinalize(_)]
        ));
        if expected_preparations == 2 {
            assert!(matches!(
                prepared[1].as_slice(),
                [BaseTaskImpl::Undelegate(_), BaseTaskImpl::BaseAction(_)]
            ));
        }
        assert!(executor.actions_callback_executor.0.lock().unwrap().is_empty());
        let disposed: Vec<_> = report
            .junk
            .iter()
            .filter(|strategy| !strategy.optimized_tasks.is_empty())
            .collect();
        assert_eq!(disposed.len(), 2);
        assert!(matches!(
            disposed[1].optimized_tasks.as_slice(),
            [BaseTaskImpl::Undelegate(_), BaseTaskImpl::BaseAction(_)]
        ));
        assert_eq!(disposed[0].lookup_tables_keys, vec![RESERVED_ALT_KEY]);
        assert_eq!(
            report
                .junk
                .iter()
                .filter(|strategy| strategy.lookup_tables_keys.contains(&RESERVED_ALT_KEY))
                .count(),
            expected_preparations,
        );
    }
}
