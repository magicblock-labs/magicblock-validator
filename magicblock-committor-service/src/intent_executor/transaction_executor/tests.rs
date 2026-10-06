use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use magicblock_core::{
    intent::{
        types::CommittedAccount, BaseAction, BaseActionCallback, ProgramArgs,
    },
    traits::{ActionResult, CallbackScheduleError},
};
use magicblock_rpc_client::MagicblockRpcClient;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_rpc_client::{
    mock_sender::MocksMap, nonblocking::rpc_client::RpcClient,
};
use solana_rpc_client_api::request::RpcRequest;

use super::*;
use crate::{
    intent_executor::{
        task_info_fetcher::{CacheTaskInfoFetcher, RpcTaskInfoFetcher},
        IntentExecutionResult,
    },
    persist::IntentPersisterImpl,
    tasks::{
        utils::{
            create_action_tasks, create_commit_finalize_task, TransactionUtils,
        },
        UndelegateTask,
    },
    transaction_preparator::{
        delivery_preparator::{BufferExecutionError, DeliveryPreparatorResult},
        error::{PreparatorResult, TransactionPreparatorError},
    },
    transactions::PreparedMessage,
};

const ACCOUNT: Pubkey = Pubkey::new_from_array([7; 32]);
const RESERVED_ALT_KEY: Pubkey = Pubkey::new_from_array([9; 32]);

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
    async fn prepare_for_strategy<P: IntentPersister>(
        &self,
        authority: &Keypair,
        strategy: &mut TransactionStrategy,
        _: &Option<P>,
    ) -> PreparatorResult<PreparedMessage> {
        self.prepared
            .lock()
            .unwrap()
            .push(strategy.optimized_tasks.clone());
        // Represent a reservation made before preparation succeeds or fails.
        strategy.lookup_tables_keys = vec![RESERVED_ALT_KEY];
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Preparation::Ready);
        match step {
            Preparation::Fail => {
                Err(TransactionPreparatorError::FailedToFitError)
            }
            Preparation::Wait => futures_util::future::pending().await,
            Preparation::Ready => {
                let mut instructions = TransactionUtils::budget_instructions(
                    TransactionUtils::tasks_compute_units(
                        &strategy.optimized_tasks,
                    ),
                    0,
                    TransactionUtils::tasks_accounts_size_budget(
                        &strategy.optimized_tasks,
                    ),
                )
                .to_vec();
                instructions.extend(TransactionUtils::tasks_instructions(
                    &authority.pubkey(),
                    &strategy.optimized_tasks,
                ));
                // The RPC mock decodes legacy messages. Keep the same budget
                // instruction offset used by production versioned messages.
                Ok(PreparedMessage::Versioned(VersionedMessage::Legacy(
                    Message::new(&instructions, Some(&authority.pubkey())),
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
    IntentExecutorImpl<RecordingPreparator, RpcTaskInfoFetcher, Callbacks>;

fn executor(
    steps: Vec<Preparation>,
    errors: &[Option<&str>],
    timeout: Duration,
) -> Executor {
    let mut mocks = MocksMap::default();
    for error in errors {
        let err = error.unwrap_or("null");
        let status = error.map_or_else(
            || r#"{"Ok":null}"#.to_string(),
            |err| format!(r#"{{"Err":{err}}}"#),
        );
        mocks.insert(
            RpcRequest::GetSignatureStatuses,
            format!(
                r#"{{
            "context": {{"slot": 1}},
            "value": [{{"slot": 1, "confirmations": null, "err": {err},
                       "status": {status}, "confirmationStatus": "finalized"}}]
        }}"#
            )
            .parse()
            .unwrap(),
        );
    }
    let rpc = MagicblockRpcClient::from(RpcClient::new_mock_with_mocks_map(
        "succeeds", mocks,
    ));
    magicblock_program::validator::generate_validator_authority_if_needed();
    IntentExecutorImpl::new(
        rpc.clone(),
        RecordingPreparator {
            steps: Arc::new(Mutex::new(steps.into())),
            ..Default::default()
        },
        Arc::new(CacheTaskInfoFetcher::new(RpcTaskInfoFetcher::new(rpc))),
        Callbacks::default(),
        timeout,
    )
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
    create_action_tasks(&[BaseAction {
        id: 0,
        destination_program: Pubkey::new_unique(),
        source_program: None,
        escrow_authority: Pubkey::new_unique(),
        account_metas_per_program: vec![],
        data_per_program: ProgramArgs {
            data: vec![],
            escrow_index: 0,
        },
        compute_units: 10_000,
        callback: Some(BaseActionCallback {
            destination_program: Pubkey::new_unique(),
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
    StrategyExecutionMode::TwoStage {
        commit_stage: strategy(vec![commit()]),
        finalize_stage: strategy(follow_up),
    }
}

fn instruction_error(error: &str) -> String {
    format!(
        r#"{{"InstructionError":[{},{error}]}}"#,
        TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT
    )
}

#[tokio::test(start_paused = true)]
async fn pending_callbacks_time_out_while_first_transaction_is_preparing() {
    let executor =
        executor(vec![Preparation::Wait], &[], Duration::from_millis(10));
    let mut report = IntentExecutionReport::default();
    let output = TransactionExecutor::new(
        &executor,
        &mut report,
        42,
        &[ACCOUNT],
        split(vec![undelegate(), action()]),
    )
    .execute(&None::<IntentPersisterImpl>)
    .await
    .unwrap();
    assert!(matches!(output, ExecutionOutput::TwoStage { .. }));
    let calls = executor.actions_callback_executor.0.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(matches!(calls[0], (None, Err(ActionError::TimeoutError))));
    let prepared = executor.transaction_preparator.prepared.lock().unwrap();
    assert_eq!(
        prepared.len(),
        3,
        "cancelled preparation, commit retry, then undelegation"
    );
    assert!(matches!(
        prepared[2].as_slice(),
        [BaseTaskImpl::Undelegate(_)]
    ));
    assert!(report.junk().iter().any(|strategy| strategy
        .lookup_tables_keys
        .contains(&RESERVED_ALT_KEY)));
}

#[tokio::test(start_paused = true)]
async fn timeout_keeps_attempts_already_spent_on_current_transaction() {
    let executor =
        executor(vec![Preparation::Wait], &[], Duration::from_millis(10));
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
        .execute_with_timeout(&None::<IntentPersisterImpl>)
        .await
        .unwrap();
    assert_eq!(runner.current_attempt, 10);
    assert_eq!(
        executor.actions_callback_executor.0.lock().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn preparation_failure_surrenders_current_and_pending_resources() {
    let executor =
        executor(vec![Preparation::Fail], &[], Duration::from_secs(60));
    let mut report = IntentExecutionReport::default();
    let result = TransactionExecutor::new(
        &executor,
        &mut report,
        42,
        &[ACCOUNT],
        split(vec![undelegate(), action()]),
    )
    .execute(&None::<IntentPersisterImpl>)
    .await;
    assert!(matches!(
        result,
        Err(IntentExecutorError::FailedCommitPreparationError(_))
    ));
    assert_eq!(report.junk().len(), 2);
    assert_eq!(report.junk()[0].lookup_tables_keys, vec![RESERVED_ALT_KEY]);
    assert_eq!(report.junk()[1].optimized_tasks.len(), 2);
    assert!(executor
        .actions_callback_executor
        .0
        .lock()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn nonce_fetch_failure_surrenders_both_strategies() {
    let nonce_error = instruction_error(&format!(
        r#"{{"Custom":{}}}"#,
        dlp_api::error::DlpError::NonceOutOfOrder as u32
    ));
    let executor =
        executor(vec![], &[Some(&nonce_error)], Duration::from_secs(60));
    let mut report = IntentExecutionReport::default();
    // The RPC mock has no delegation record, so nonce recovery fails after
    // preparing the first transaction and receiving its on-chain nonce error.
    let result = TransactionExecutor::new(
        &executor,
        &mut report,
        42,
        &[ACCOUNT],
        split(vec![undelegate()]),
    )
    .execute(&None::<IntentPersisterImpl>)
    .await;
    assert!(
        matches!(result, Err(IntentExecutorError::TaskBuilderError(_))),
        "{result:?}"
    );
    assert_eq!(
        executor
            .transaction_preparator
            .prepared
            .lock()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(report.junk().len(), 2);
    assert_eq!(report.junk()[0].lookup_tables_keys, vec![RESERVED_ALT_KEY]);
    assert!(matches!(
        report.junk()[1].optimized_tasks.as_slice(),
        [BaseTaskImpl::Undelegate(_)]
    ));
}

#[tokio::test]
async fn follow_up_preparation_failure_does_not_retry_confirmed_commit() {
    let executor = executor(
        vec![Preparation::Ready, Preparation::Fail],
        &[],
        Duration::from_secs(60),
    );
    let mut report = IntentExecutionReport::default();
    let result = TransactionExecutor::new(
        &executor,
        &mut report,
        42,
        &[ACCOUNT],
        split(vec![undelegate()]),
    )
    .execute(&None::<IntentPersisterImpl>)
    .await;
    assert!(matches!(
        result,
        Err(IntentExecutorError::FailedFinalizePreparationError(_))
    ));
    let result = IntentExecutionResult {
        inner: result,
        patched_errors: vec![],
        callbacks_report: vec![],
    };
    assert!(!result.is_retriable(true));
    let prepared = executor.transaction_preparator.prepared.lock().unwrap();
    assert_eq!(prepared.len(), 2);
    assert!(matches!(
        prepared[0].as_slice(),
        [BaseTaskImpl::CommitFinalize(_)]
    ));
    assert!(matches!(
        prepared[1].as_slice(),
        [BaseTaskImpl::Undelegate(_)]
    ));
    assert_eq!(
        report
            .junk()
            .iter()
            .filter(|strategy| strategy
                .lookup_tables_keys
                .contains(&RESERVED_ALT_KEY))
            .count(),
        2
    );
}

#[tokio::test]
async fn empty_follow_up_preserves_action_and_undelegation_outcomes() {
    for failed_action in [true, false] {
        let error = instruction_error(r#"{"Custom":1}"#);
        let executor =
            executor(vec![], &[None, Some(&error)], Duration::from_secs(60));
        let mut report = IntentExecutionReport::default();
        let follow_up = if failed_action {
            action()
        } else {
            undelegate()
        };
        let result = TransactionExecutor::new(
            &executor,
            &mut report,
            42,
            &[ACCOUNT],
            split(vec![follow_up]),
        )
        .execute(&None::<IntentPersisterImpl>)
        .await;
        assert_eq!(
            executor
                .transaction_preparator
                .prepared
                .lock()
                .unwrap()
                .len(),
            2,
            "must not prepare an empty retry"
        );
        if failed_action {
            assert!(
                matches!(result, Ok(ExecutionOutput::TwoStage { commit_signature, finalize_signature }) if commit_signature == finalize_signature),
                "{result:?}"
            );
            let calls = executor.actions_callback_executor.0.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, Err(ActionError::ActionsError(..))));
            assert_eq!(report.patched_errors().len(), 1);
        } else {
            assert!(
                matches!(
                    result,
                    Err(IntentExecutorError::FailedToFinalizeError {
                        commit_signature: Some(_),
                        ..
                    })
                ),
                "{result:?}"
            );
            assert!(!result.unwrap_err().is_transient());
        }
    }
}
