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
    intent_executor::task_info_fetcher::{
        CacheTaskInfoFetcher, RpcTaskInfoFetcher,
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
    error: Option<&str>,
    timeout: Duration,
) -> Executor {
    let mut mocks = MocksMap::default();
    if let Some(error) = error {
        mocks.insert(
            RpcRequest::GetSignatureStatuses,
            format!(
                r#"{{
            "context": {{"slot": 1}},
            "value": [{{"slot": 1, "confirmations": null, "err": {error},
                       "status": {{"Err":{error}}}, "confirmationStatus": "finalized"}}]
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

#[tokio::test(start_paused = true)]
async fn timeout_skips_empty_follow_up_and_keeps_undelegation() {
    // - `[Action]` → `[]`: skip the empty follow-up.
    // - `[Undelegate, Action]` → `[Undelegate]`: execute the undelegation.
    for timeout_during_commit in [true, false] {
        for has_undelegation in [false, true] {
            let steps = if timeout_during_commit {
                vec![Preparation::Wait]
            } else {
                vec![Preparation::Ready, Preparation::Wait]
            };
            let follow_up = if has_undelegation {
                vec![undelegate(), action()]
            } else {
                vec![action()]
            };
            let executor = executor(steps, None, Duration::from_secs(60));
            let mut report = IntentExecutionReport::default();
            let output = TransactionExecutor::new(
                &executor,
                &mut report,
                42,
                &[ACCOUNT],
                split(follow_up),
            )
            .execute(&None::<IntentPersisterImpl>)
            .await
            .unwrap();
            let ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } = output
            else {
                panic!("expected two-stage output");
            };
            let calls = executor.actions_callback_executor.0.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0], (None, Err(ActionError::TimeoutError))));
            let prepared =
                executor.transaction_preparator.prepared.lock().unwrap();
            assert!(
                prepared.iter().all(|tasks| !tasks.is_empty()),
                "must not prepare an empty follow-up"
            );
            assert_eq!(prepared.len(), 2 + usize::from(has_undelegation));
            assert_eq!(
                prepared
                    .iter()
                    .filter(|tasks| matches!(
                        tasks.as_slice(),
                        [BaseTaskImpl::CommitFinalize(_)]
                    ))
                    .count(),
                if timeout_during_commit { 2 } else { 1 },
                "only a cancelled commit preparation may be retried"
            );
            if has_undelegation {
                assert!(matches!(
                    prepared.last().unwrap().as_slice(),
                    [BaseTaskImpl::Undelegate(_)]
                ));
            } else {
                assert_eq!(commit_signature, finalize_signature);
            }
            assert_eq!(
                report
                    .junk()
                    .iter()
                    .filter(|strategy| strategy
                        .lookup_tables_keys
                        .contains(&RESERVED_ALT_KEY))
                    .count(),
                prepared.len(),
                "every preparation's ALT reservation must be surrendered"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_keeps_attempts_already_spent_on_current_transaction() {
    let executor =
        executor(vec![Preparation::Wait], None, Duration::from_millis(10));
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
async fn execution_failure_surrenders_current_and_pending_resources() {
    let nonce_error = format!(
        r#"{{"InstructionError":[{},{{"Custom":{}}}]}}"#,
        TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT,
        dlp_api::error::DlpError::NonceOutOfOrder as u32
    );
    // The RPC mock has no delegation record, so nonce recovery fails after
    // preparing the first transaction and receiving its on-chain nonce error.
    for (steps, error) in [
        (vec![Preparation::Fail], None),
        (vec![Preparation::Ready], Some(nonce_error.as_str())),
        (vec![Preparation::Ready, Preparation::Fail], None),
    ] {
        let expected_preparations = steps.len();
        let executor = executor(steps, error, Duration::from_secs(60));
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
        assert!(executor
            .actions_callback_executor
            .0
            .lock()
            .unwrap()
            .is_empty());
        let disposed: Vec<_> = report
            .junk()
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
                .junk()
                .iter()
                .filter(|strategy| strategy
                    .lookup_tables_keys
                    .contains(&RESERVED_ALT_KEY))
                .count(),
            expected_preparations,
        );
    }
}
