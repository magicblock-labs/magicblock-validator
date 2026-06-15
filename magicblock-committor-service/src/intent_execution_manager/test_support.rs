#![cfg(test)]
//! Shared test helpers for intent execution manager/engine tests.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_signature::Signature;
use solana_signer::SignerError;
use solana_transaction_error::TransactionError;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    intent_execution_manager::{
        db::DummyDB,
        intent_execution_engine::{IntentExecutionEngine, ResultSubscriber},
        IntentExecutionManager,
    },
    intent_executor::{
        error::{
            IntentExecutorError as ExecutorError, InternalError,
            TransactionStrategyExecutionError,
        },
        intent_executor_factory::IntentExecutorFactory,
        ExecutionOutput, IntentExecutionResult, IntentExecutor,
    },
    persist::IntentPersisterImpl,
    test_utils,
    transaction_preparator::delivery_preparator::BufferExecutionError,
};

pub type TestEngine = IntentExecutionEngine<
    DummyDB,
    IntentPersisterImpl,
    MockIntentExecutorFactory,
>;

pub struct SpawnedEngine {
    pub shutdown: CancellationToken,
    pub idle_rx: watch::Receiver<bool>,
    pub result_subscriber: ResultSubscriber,
}

pub fn setup_engine(
    should_fail: bool,
) -> (mpsc::Sender<ScheduledIntentBundle>, TestEngine) {
    setup_engine_with_factory(if should_fail {
        MockIntentExecutorFactory::new_failing()
    } else {
        MockIntentExecutorFactory::new()
    })
}

pub fn setup_engine_with_factory(
    factory: MockIntentExecutorFactory,
) -> (mpsc::Sender<ScheduledIntentBundle>, TestEngine) {
    test_utils::init_test_logger();

    let (sender, receiver) = mpsc::channel(1000);
    let db = Arc::new(DummyDB::new());
    let worker = IntentExecutionEngine::new(
        db,
        factory,
        None::<IntentPersisterImpl>,
        receiver,
    );

    (sender, worker)
}

pub fn new_test_intent_execution_manager(
    factory: MockIntentExecutorFactory,
) -> IntentExecutionManager<DummyDB> {
    let db = Arc::new(DummyDB::new());
    let (sender, receiver) = mpsc::channel(1000);
    let shutdown = CancellationToken::new();
    let (idle_tx, idle_rx) = watch::channel(false);
    let worker = IntentExecutionEngine::new(
        db.clone(),
        factory,
        None::<IntentPersisterImpl>,
        receiver,
    );
    let result_subscriber = worker.spawn(shutdown.clone(), idle_tx);

    IntentExecutionManager {
        db,
        intent_sender: sender,
        result_subscriber,
        shutdown,
        idle_rx,
    }
}

pub fn spawn_engine(worker: TestEngine) -> SpawnedEngine {
    let shutdown = CancellationToken::new();
    let (idle_tx, idle_rx) = watch::channel(false);
    let result_subscriber = worker.spawn(shutdown.clone(), idle_tx);

    SpawnedEngine {
        shutdown,
        idle_rx,
        result_subscriber,
    }
}

pub async fn wait_idle(idle_rx: &mut watch::Receiver<bool>) {
    if *idle_rx.borrow() {
        return;
    }
    idle_rx
        .changed()
        .await
        .expect("engine idle signal dropped before drain completed");
    assert!(*idle_rx.borrow());
}

pub struct MockIntentExecutorFactory {
    should_fail: bool,
    execution_delay: Duration,
    active_tasks: Option<Arc<AtomicUsize>>,
    max_concurrent: Option<Arc<AtomicUsize>>,
}

impl MockIntentExecutorFactory {
    pub fn new() -> Self {
        Self {
            should_fail: false,
            execution_delay: Duration::from_millis(50),
            active_tasks: None,
            max_concurrent: None,
        }
    }

    pub fn new_failing() -> Self {
        Self {
            should_fail: true,
            execution_delay: Duration::from_millis(50),
            active_tasks: None,
            max_concurrent: None,
        }
    }

    pub fn with_execution_delay(mut self, execution_delay: Duration) -> Self {
        self.execution_delay = execution_delay;
        self
    }

    pub fn with_concurrency_tracking(
        &mut self,
        active_tasks: &Arc<AtomicUsize>,
        max_concurrent: &Arc<AtomicUsize>,
    ) {
        self.active_tasks = Some(active_tasks.clone());
        self.max_concurrent = Some(max_concurrent.clone());
    }
}

impl IntentExecutorFactory for MockIntentExecutorFactory {
    type Executor = MockIntentExecutor;

    fn create_instance(&self) -> Self::Executor {
        MockIntentExecutor {
            should_fail: self.should_fail,
            execution_delay: self.execution_delay,
            active_tasks: self.active_tasks.clone(),
            max_concurrent: self.max_concurrent.clone(),
        }
    }
}

pub struct MockIntentExecutor {
    should_fail: bool,
    execution_delay: Duration,
    active_tasks: Option<Arc<AtomicUsize>>,
    max_concurrent: Option<Arc<AtomicUsize>>,
}

impl MockIntentExecutor {
    fn on_task_started(&self) {
        if let (Some(active), Some(max)) =
            (&self.active_tasks, &self.max_concurrent)
        {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;

            let mut observed_max = max.load(Ordering::SeqCst);
            while current > observed_max {
                match max.compare_exchange_weak(
                    observed_max,
                    current,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(x) => observed_max = x,
                }
            }
        }
    }

    fn on_task_finished(&self) {
        if let Some(active) = &self.active_tasks {
            active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[async_trait]
impl IntentExecutor for MockIntentExecutor {
    async fn execute<P: crate::persist::IntentPersister>(
        &mut self,
        _base_intent: ScheduledIntentBundle,
        _persister: Option<P>,
    ) -> IntentExecutionResult {
        self.on_task_started();
        tokio::time::sleep(self.execution_delay).await;

        let result = if self.should_fail {
            IntentExecutionResult {
                inner: Err(ExecutorError::FailedToCommitError {
                    err: TransactionStrategyExecutionError::InternalError(
                        InternalError::SignerError(SignerError::Custom(
                            "oops".to_string(),
                        )),
                    ),
                    signature: None,
                }),
                patched_errors: vec![
                    TransactionStrategyExecutionError::ActionsError(
                        TransactionError::AccountNotFound,
                        None,
                    ),
                ],
                callbacks_report: vec![],
            }
        } else {
            IntentExecutionResult {
                inner: Ok(ExecutionOutput::TwoStage {
                    commit_signature: Signature::default(),
                    finalize_signature: Signature::default(),
                }),
                patched_errors: vec![],
                callbacks_report: vec![],
            }
        };

        self.on_task_finished();
        result
    }

    async fn cleanup(self) -> Result<(), BufferExecutionError> {
        Ok(())
    }
}
