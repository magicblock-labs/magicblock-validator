use std::{
    marker::PhantomData,
    ops::Deref,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::stream::FuturesUnordered;
use magicblock_core::traits::CallbackScheduleError;
use magicblock_metrics::metrics;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use solana_signature::Signature;
use tokio::{
    sync::{broadcast, OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
    time::sleep,
};
use tokio_stream::StreamExt;
use tracing::{error, info, instrument, trace, warn};

#[cfg(feature = "dev-context-only-utils")]
use crate::tasks::task_strategist::TransactionStrategy;
use crate::{
    intent_engine::{
        db::BacklogDB,
        intent_channel::{IntentScheduleError, IntentStream},
        intent_scheduler::{IntentScheduler, POISONED_INNER_MSG},
    },
    intent_executor::{
        error::{IntentExecutorError, IntentExecutorResult},
        intent_executor_factory::IntentExecutorBuilder,
        strategy_executor::error::TransactionStrategyExecutionError,
        ExecutionOutput, IntentExecutionResult,
    },
    transaction_preparator::TransactionPreparator,
};

const SEMAPHORE_CLOSED_MSG: &str = "Executors semaphore closed!";
/// Max number of executors that can send messages in parallel to Base layer
const MAX_EXECUTORS: u8 = 50;
/// Max intents concurrently sleeping in retry backoff. Retries release their
/// executor slot while sleeping; this cap keeps the total task population
/// bounded when many intents fail together
const MAX_SLEEPING_RETRIERS: usize = 5_000;
/// Max executions of an intent whose failures were transient
const MAX_INTENT_ATTEMPTS: u32 = 3;
/// Backoff between intent attempts, scaled linearly by attempt number
const INTENT_RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// Concurrency limits shared between the engine and its executor tasks
struct ExecutionLimits {
    executors: Arc<Semaphore>,
    retries: Arc<Semaphore>,
}

pub type PatchedErrors = Vec<TransactionStrategyExecutionError>;

#[derive(Clone, Debug)]
pub struct BroadcastedIntentExecutionResult {
    pub inner: Result<ExecutionOutput, Arc<IntentExecutorError>>,
    pub id: u64,
    pub patched_errors: Arc<PatchedErrors>,
    pub callbacks_report: Vec<Result<Signature, Arc<CallbackScheduleError>>>,
    #[cfg(feature = "dev-context-only-utils")]
    pub successful_transaction_strategies: Vec<TransactionStrategy>,
}

impl BroadcastedIntentExecutionResult {
    fn new(id: u64, execution_result: IntentExecutionResult) -> Self {
        let inner = execution_result.inner.map_err(Arc::new);
        let patched_errors = execution_result.patched_errors.into();
        let callbacks_report = execution_result
            .callbacks_report
            .into_iter()
            .map(|el| el.map_err(Arc::new))
            .collect();

        Self {
            id,
            patched_errors,
            inner,
            callbacks_report,
            #[cfg(feature = "dev-context-only-utils")]
            successful_transaction_strategies: execution_result
                .successful_transaction_strategies,
        }
    }
}

impl Deref for BroadcastedIntentExecutionResult {
    type Target = Result<ExecutionOutput, Arc<IntentExecutorError>>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Struct that exposes only `subscribe` method of `broadcast::Sender` for better isolation
pub struct ResultSubscriber(
    broadcast::Sender<BroadcastedIntentExecutionResult>,
);
impl ResultSubscriber {
    pub fn subscribe(
        &self,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        self.0.subscribe()
    }
}

pub(crate) struct IntentExecutionEngine<D, F, T> {
    intent_stream: IntentStream<D>,
    executor_builder: Arc<F>,

    inner: Arc<Mutex<IntentScheduler>>,
    running_executors: FuturesUnordered<JoinHandle<()>>,
    executors_semaphore: Arc<Semaphore>,
    retries_semaphore: Arc<Semaphore>,
    _phantom_data: PhantomData<T>,
}

impl<D, F, T> IntentExecutionEngine<D, F, T>
where
    D: BacklogDB,
    T: TransactionPreparator,
    F: IntentExecutorBuilder<T> + Send + Sync + 'static,
{
    pub fn new(intent_stream: IntentStream<D>, executor_builder: F) -> Self {
        Self {
            intent_stream,
            executor_builder: Arc::new(executor_builder),
            running_executors: FuturesUnordered::new(),
            executors_semaphore: Arc::new(Semaphore::new(
                MAX_EXECUTORS as usize,
            )),
            retries_semaphore: Arc::new(Semaphore::new(MAX_SLEEPING_RETRIERS)),
            inner: Arc::new(Mutex::new(IntentScheduler::new())),
            _phantom_data: PhantomData,
        }
    }

    /// Spawns `main_loop` and return `Receiver` listening to results
    pub fn spawn(self) -> ResultSubscriber {
        let (result_sender, _) = broadcast::channel(100);
        tokio::spawn(self.main_loop(result_sender.clone()));

        ResultSubscriber(result_sender)
    }

    /// Main loop that:
    /// 1. Handles & schedules incoming intents
    /// 2. Finds available executor
    /// 3. Spawns execution of scheduled intent
    #[instrument(skip(self, result_sender))]
    async fn main_loop(
        mut self,
        result_sender: broadcast::Sender<BroadcastedIntentExecutionResult>,
    ) {
        loop {
            let intent = match self.next_scheduled_intent().await {
                Ok(value) => value,
                Err(IntentScheduleError::ChannelClosed) => {
                    info!("Channel closed, exiting");
                    break;
                }
                Err(IntentScheduleError::DBError(err)) => {
                    // TODO(edwin): add to alert as this is critical error
                    error!(error = ?err, "Failed to fetch intent");
                    continue;
                }
            };
            let Some(intent) = intent else {
                // We couldn't pick up intent for execution due to:
                // 1. All executors are currently busy
                // 2. All intents are blocked and none could be executed at the moment
                trace!("Could not schedule any intents");
                continue;
            };

            // Waiting until there's available executor
            let permit = self
                .executors_semaphore
                .clone()
                .acquire_owned()
                .await
                .expect(SEMAPHORE_CLOSED_MSG);

            // Spawn executor
            let executor_factory = self.executor_builder.clone();
            let inner = self.inner.clone();
            let limits = ExecutionLimits {
                executors: self.executors_semaphore.clone(),
                retries: self.retries_semaphore.clone(),
            };

            let handle = tokio::spawn(Self::execute(
                executor_factory,
                intent,
                inner,
                limits,
                permit,
                result_sender.clone(),
            ));

            self.running_executors.push(handle);
            metrics::set_committor_executors_busy_count(
                self.running_executors.len() as i64,
            );
        }
    }

    /// Returns [`OutboxIntentBundle`] or None if all intents are blocked
    #[instrument(skip(self))]
    async fn next_scheduled_intent(
        &mut self,
    ) -> Result<Option<OutboxIntentBundle>, IntentScheduleError> {
        // Limit on number of intents that can be stored in scheduler
        const SCHEDULER_CAPACITY: usize = 1000;

        let can_receive = || {
            let num_blocked_intents = self
                .inner
                .lock()
                .expect(POISONED_INNER_MSG)
                .intents_blocked();
            if num_blocked_intents < SCHEDULER_CAPACITY {
                true
            } else {
                warn!(blocked_count = num_blocked_intents, "Capacity exceeded");
                false
            }
        };

        let running_executors = &mut self.running_executors;
        let intent_stream = &mut self.intent_stream;
        let intent = tokio::select! {
            // Notify polled first to prioritize unblocked intents over new one
            biased;
            Some(result) = running_executors.next() => {
                if let Err(err) = result {
                    error!(error = ?err, "Executor failed");
                };
                trace!("Worker executed intent bundle, fetching new available one");
                self.inner.lock().expect(POISONED_INNER_MSG).pop_next_scheduled_intent()
            },
            result = Self::get_new_intent(intent_stream), if can_receive() => {
                let intent = result?;
                self.inner.lock().expect(POISONED_INNER_MSG).schedule(intent)
            },
            else => {
                // Shouldn't be possible:
                // 1. If no executors spawned -> we can receive
                // 2. If can't receive ->  there are MAX_EXECUTORS running executors
                // We can't receive new message as there's no available Executor
                // that could pick up the task.
                unreachable!("next_scheduled_intent")
            }
        };

        Ok(intent)
    }

    /// Returns [`ScheduledIntentBundle`] from external channel
    async fn get_new_intent(
        intent_stream: &mut IntentStream<D>,
    ) -> Result<OutboxIntentBundle, IntentScheduleError> {
        intent_stream
            .next()
            .await
            .ok_or(IntentScheduleError::ChannelClosed)?
            .map_err(IntentScheduleError::DBError)
    }

    /// Wrapper on [`IntentExecutor`] that handles its results and drops execution permit.
    /// Transient failures are retried with a fresh executor while the scheduler
    /// keeps conflicting intents blocked, preserving per-account commit order.
    #[instrument(skip(executor_factory, intent, inner_scheduler, limits, execution_permit, result_sender), fields(intent_id = intent.id))]
    async fn execute(
        executor_factory: Arc<F>,
        intent: OutboxIntentBundle,
        inner_scheduler: Arc<Mutex<IntentScheduler>>,
        limits: ExecutionLimits,
        execution_permit: OwnedSemaphorePermit,
        result_sender: broadcast::Sender<BroadcastedIntentExecutionResult>,
    ) {
        let instant = Instant::now();

        let (result, execution_permit) = Self::execute_with_retries(
            executor_factory,
            &intent,
            limits,
            execution_permit,
        )
        .await;

        // Report
        let is_err = result.inner.as_ref().inspect_err(|err| {
            error!(intent_id = intent.id, error = ?err, "Failed to execute intent bundle");
        }).is_err();
        Self::execution_metrics(instant.elapsed(), &intent, &result.inner);
        let broadcasted_result =
            BroadcastedIntentExecutionResult::new(intent.id, result);
        if let Err(err) = result_sender.send(broadcasted_result) {
            warn!(error = ?err, "No result listeners");
        }

        // Lock intent's pubkeys. This will prevent future intents from execution
        // if future intent overlaps with failed one(current)
        if is_err {
            let pubkeys = intent.get_all_committed_pubkeys();
            warn!(pubkeys = ?pubkeys, "Intents for following pubkeys are now blocked.");
            return;
        }
        // That would
        // Remove executed task from Scheduler to unblock other intents
        // SAFETY: Self::execute is called ONLY after IntentScheduler
        // successfully is able to schedule execution of some Intent
        // that means that the same Intent is SAFE to complete
        inner_scheduler
            .lock()
            .expect(POISONED_INNER_MSG)
            .complete(&intent)
            .expect("Valid completion of previously scheduled message");

        // Free worker
        drop(execution_permit);
    }

    /// Executes an intent, retrying plausibly-transient failures with a
    /// fresh executor. Returns the final result together with whichever
    /// execution permit is still held (`None` only if the executors
    /// semaphore closed mid-retry).
    async fn execute_with_retries(
        executor_factory: Arc<F>,
        intent: &OutboxIntentBundle,
        limits: ExecutionLimits,
        execution_permit: OwnedSemaphorePermit,
    ) -> (IntentExecutionResult, Option<OwnedSemaphorePermit>) {
        // Commit tasks give on-chain dedup (commit nonce) to re-executed
        // sends; action-only intents can double-execute if their transaction
        // landed unobserved, so they only retry pre-send failures
        let has_dedup_guard = !intent.get_all_committed_pubkeys().is_empty();

        let mut attempt = 0;
        let mut execution_permit = Some(execution_permit);
        let result = loop {
            attempt += 1;
            // TODO(edwin): reconcile intent on retry in the future
            let executor =
                executor_factory.create_instance(intent.status().clone());
            let (result, cleanup_handle) =
                executor.execute(intent.inner.clone()).await;

            tokio::spawn(async move {
                if let Err(err) = cleanup_handle.clean().await {
                    error!(error = ?err, "Failed to cleanup after intent");
                }
            });

            // break early if we can't retry anymore
            if attempt >= MAX_INTENT_ATTEMPTS
                || !result.is_retriable(has_dedup_guard)
            {
                break result;
            }

            // Sleeping retries release their executor slot but must stay
            // bounded; without a free retry slot the failure is terminal
            let Ok(retry_permit) = limits.retries.clone().try_acquire_owned()
            else {
                warn!(intent_id = intent.id, "Retry capacity exhausted");
                break result;
            };

            if let Err(err) = &result.inner {
                warn!(intent_id = intent.id, attempt, error = ?err, "Transient intent failure, retrying");
            }

            // Release the executor slot during backoff so unrelated intents
            // keep executing while this one waits out the outage.
            // Per-intent jitter decorrelates synchronized retry bursts when
            // many intents fail together during an outage
            let jitter = Duration::from_millis((intent.id % 8) * 125);
            drop(execution_permit.take());
            sleep(INTENT_RETRY_BACKOFF * attempt + jitter).await;
            execution_permit =
                match limits.executors.clone().acquire_owned().await {
                    Ok(permit) => Some(permit),
                    Err(_) => {
                        error!(SEMAPHORE_CLOSED_MSG);
                        break result;
                    }
                };
            drop(retry_permit);
        };

        (result, execution_permit)
    }

    /// Records metrics related to intent execution
    fn execution_metrics(
        execution_time: Duration,
        intent: &OutboxIntentBundle,
        result: &IntentExecutorResult<ExecutionOutput>,
    ) {
        const EXECUTION_TIME_THRESHOLD: f64 = 5.0;
        const INTENT_BUNDLE_LABEL: &str = "intent_bundle";

        let intent_execution_secs = execution_time.as_secs_f64();
        metrics::observe_committor_intent_execution_time_histogram(
            intent_execution_secs,
            &INTENT_BUNDLE_LABEL,
            result,
        );
        if let Err(ref err) = result {
            metrics::inc_committor_failed_intents_count(
                &INTENT_BUNDLE_LABEL,
                err,
            );
        }

        // Loki alerts
        if intent_execution_secs >= EXECUTION_TIME_THRESHOLD {
            info!(duration_secs = intent_execution_secs, result = ?result, "Intent bundle took too long to execute");
        } else {
            trace!(duration_secs = intent_execution_secs, result = ?result, "Intent bundle execution time");
        }

        // Alert
        if intent.has_undelegate_intent() && result.is_err() {
            warn!(stuck_accounts = ?intent.get_undelegate_intent_pubkeys(), "Intent execution resulted in stuck accounts");
        }
    }
}

/// Worker tests
#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use magicblock_rpc_client::MagicBlockRpcClientError;
    use solana_keypair::Keypair;
    use solana_message::VersionedMessage;
    use solana_pubkey::{pubkey, Pubkey};
    use solana_signature::Signature;
    use solana_signer::SignerError;
    use solana_transaction_error::TransactionError;
    use tokio::time::sleep;

    use super::*;
    use crate::{
        intent_engine::{
            db::{BacklogDB, DummyDB},
            intent_channel::{channel, IntentScheduleHandle},
            intent_scheduler::{create_test_intent, create_test_intent_bundle},
        },
        intent_executor::{
            cleanup_handle::CleanupHandle,
            error::{IntentExecutorError as ExecutorError, InternalError},
            intent_executor_factory::IntentExecutorBuilder,
            IntentExecutionResult, IntentExecutor,
        },
        tasks::task_strategist::TransactionStrategy,
        test_utils,
        transaction_preparator::{
            delivery_preparator::{
                BufferExecutionError, DeliveryPreparatorResult,
            },
            error::PreparatorResult,
            TransactionPreparator,
        },
    };

    struct MockTransactionPreparator;

    #[async_trait]
    impl TransactionPreparator for MockTransactionPreparator {
        async fn prepare_for_strategy(
            &self,
            _authority: &Keypair,
            _transaction_strategy: &mut TransactionStrategy,
        ) -> PreparatorResult<VersionedMessage> {
            unimplemented!()
        }

        async fn cleanup_for_strategy(
            &self,
            _authority: &Keypair,
            _transaction_strategy: &TransactionStrategy,
            _close_buffers: bool,
        ) -> DeliveryPreparatorResult<(), BufferExecutionError> {
            Ok(())
        }
    }

    type MockIntentExecutionEngine = IntentExecutionEngine<
        DummyDB,
        MockIntentExecutorFactory,
        MockTransactionPreparator,
    >;
    fn setup_engine(
        should_fail: bool,
    ) -> (
        IntentScheduleHandle<DummyDB>,
        MockIntentExecutionEngine,
        Arc<Mutex<DummyDB>>,
    ) {
        let executor_factory = if !should_fail {
            MockIntentExecutorFactory::new()
        } else {
            MockIntentExecutorFactory::new_failing()
        };
        setup_engine_with_factory(executor_factory)
    }

    fn setup_engine_with_factory(
        executor_factory: MockIntentExecutorFactory,
    ) -> (
        IntentScheduleHandle<DummyDB>,
        MockIntentExecutionEngine,
        Arc<Mutex<DummyDB>>,
    ) {
        test_utils::init_test_logger();

        let db = Arc::new(Mutex::new(DummyDB::new()));
        let (handle, intent_stream) = channel(&db, 1000);
        let worker =
            IntentExecutionEngine::new(intent_stream, executor_factory);

        (handle, worker, db)
    }

    #[tokio::test]
    async fn test_worker_processes_messages() {
        let (sender, worker, _db) = setup_engine(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a test message
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.schedule(vec![msg.clone()]).unwrap();

        // Verify the message was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.id, 1);
    }

    #[tokio::test]
    async fn test_worker_handles_conflicting_messages() {
        let (sender, worker, _db) = setup_engine(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send two conflicting messages
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey], false);
        let msg2 = create_test_intent(2, &[pubkey], false);

        sender.schedule(vec![msg1.clone()]).unwrap();
        sender.schedule(vec![msg2.clone()]).unwrap();

        // First message should be processed immediately
        let result1 = result_receiver.recv().await.unwrap();
        assert!(result1.is_ok());
        assert_eq!(result1.id, 1);

        // Second message should be processed after first completes
        let result2 = result_receiver.recv().await.unwrap();
        assert!(result2.is_ok());
        assert_eq!(result2.id, 2);
    }

    #[tokio::test]
    async fn test_worker_handles_conflicting_bundles() {
        let (sender, worker, _db) = setup_engine(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send two conflicting messages
        let a = pubkey!("1111111111111111111111111111111111111111111");
        let b = pubkey!("21111111111111111111111111111111111111111111");
        let msg1 = create_test_intent_bundle(1, &[a], &[b]);
        let msg2 = create_test_intent(2, &[a], false);

        sender.schedule(vec![msg1.clone()]).unwrap();
        sender.schedule(vec![msg2.clone()]).unwrap();

        // First message should be processed immediately
        let result1 = result_receiver.recv().await.unwrap();
        assert!(result1.is_ok());
        assert_eq!(result1.id, 1);

        // Second message should be processed after first completes
        let result2 = result_receiver.recv().await.unwrap();
        assert!(result2.is_ok());
        assert_eq!(result2.id, 2);
    }

    #[tokio::test]
    async fn test_worker_handles_executor_failure() {
        let (sender, worker, _db) = setup_engine(true);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a test message that will fail
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.schedule(vec![msg.clone()]).unwrap();

        // Verify the failure was properly reported
        let result = result_receiver.recv().await.unwrap();
        assert!(result.inner.is_err());
        assert_eq!(result.id, 1);
        assert_eq!(
            result.patched_errors[0].to_string(),
            "User supplied actions are ill-formed: Attempt to debit an account but found no record of a prior credit.. None"
        );
        assert_eq!(
            result.inner.unwrap_err().to_string(),
            "FailedToCommitError: InternalError: SignerError: custom error: oops"
        );
    }

    #[tokio::test]
    async fn test_worker_falls_back_to_db_when_channel_empty() {
        let (_sender, worker, db) = setup_engine(false);

        // Add a message to the DB
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        db.lock().unwrap().store_intent_bundle(msg.clone()).unwrap();

        // Start worker
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Verify the message from DB was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.id, 1);
    }

    /// Tests multiple blocking messages being sent at the same time
    #[tokio::test]
    async fn test_high_throughput_message_processing() {
        const NUM_MESSAGES: usize = 20;

        let (sender, mut worker, _db) = setup_engine(false);

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        Arc::get_mut(&mut worker.executor_builder)
            .expect("factory not shared before spawn")
            .with_concurrency_tracking(&active_tasks, &max_concurrent);

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a flood of messages
        for i in 0..NUM_MESSAGES {
            let msg = create_test_intent(
                i as u64,
                &[pubkey!("1111111111111111111111111111111111111111111")],
                false,
            );
            sender.schedule(vec![msg]).unwrap();
        }

        // Process results and verify constraints
        let mut completed = 0;
        while completed < NUM_MESSAGES {
            let result = result_receiver.recv().await.unwrap();
            assert!(result.is_ok());
            // Tasks are blocking so will complete sequentially
            assert_eq!(result.id, completed as u64);
            completed += 1;
        }

        // Verify we didn't exceed concurrency limits
        let max_observed = max_concurrent.load(Ordering::SeqCst);
        assert_eq!(
            max_observed, 1,
            "Blocking messages can't execute in parallel!"
        );
    }

    /// Tests that errors from executor propagated gracefully
    #[tokio::test]
    async fn test_multiple_failures() {
        let (sender, worker, _db) = setup_engine(true); // Worker that always fails
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send several messages that will fail
        const NUM_FAILURES: usize = 10;
        for i in 0..NUM_FAILURES {
            let msg = create_test_intent(
                i as u64,
                &[pubkey!("1111111111111111111111111111111111111111111")],
                false,
            );
            sender.schedule(vec![msg]).unwrap();
        }

        // Verify all failures are processed and semaphore slots released
        for _ in 0..NUM_FAILURES {
            let result = result_receiver.recv().await.unwrap();
            assert!(result.is_err());
        }
    }

    /// Transient failures are retried with a fresh executor until success
    #[tokio::test(start_paused = true)]
    async fn test_transient_failure_retried_until_success() {
        let factory = MockIntentExecutorFactory::new_transient(2);
        let created_instances = factory.created_instances.clone();
        let (sender, worker, _db) = setup_engine_with_factory(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.schedule(vec![msg]).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(created_instances.load(Ordering::SeqCst), 3);
    }

    /// Transient failures stop being retried once attempts are exhausted
    #[tokio::test(start_paused = true)]
    async fn test_transient_failure_exhausts_attempts() {
        let factory = MockIntentExecutorFactory::new_transient(usize::MAX);
        let created_instances = factory.created_instances.clone();
        let (sender, worker, _db) = setup_engine_with_factory(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.schedule(vec![msg]).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_err());
        assert_eq!(
            created_instances.load(Ordering::SeqCst),
            MAX_INTENT_ATTEMPTS as usize
        );
    }

    /// Non-transient failures are terminal on the first attempt
    #[tokio::test]
    async fn test_non_transient_failure_not_retried() {
        let factory = MockIntentExecutorFactory::new_failing();
        let created_instances = factory.created_instances.clone();
        let (sender, worker, _db) = setup_engine_with_factory(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.schedule(vec![msg]).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_err());
        assert_eq!(created_instances.load(Ordering::SeqCst), 1);
    }

    /// Once action callbacks fired, even a transient failure must not retry
    #[tokio::test]
    async fn test_transient_failure_with_callbacks_not_retried() {
        let mut factory = MockIntentExecutorFactory::new_transient(usize::MAX);
        factory.report_callbacks_on_failure = true;
        let created_instances = factory.created_instances.clone();
        let (sender, worker, _db) = setup_engine_with_factory(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.schedule(vec![msg]).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_err());
        assert_eq!(created_instances.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_non_blocking_messages() {
        const NUM_MESSAGES: u64 = 200;

        let (sender, mut worker, _db) = setup_engine(false);

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        Arc::get_mut(&mut worker.executor_builder)
            .expect("factory not shared before spawn")
            .with_concurrency_tracking(&active_tasks, &max_concurrent);

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send messages with unique keys (non-blocking)
        let mut received_ids = HashSet::new();
        for i in 0..NUM_MESSAGES {
            let unique_pubkey = Pubkey::new_unique(); // Each message gets unique key
            let msg = create_test_intent(i, &[unique_pubkey], false);

            received_ids.insert(i);
            sender.schedule(vec![msg]).unwrap();
        }

        // Process results
        let mut completed = 0;
        while completed < NUM_MESSAGES {
            let result = result_receiver.recv().await.unwrap();
            assert!(result.is_ok());

            // Message has to be present in set
            let id = result.id;
            assert!(received_ids.remove(&id));

            completed += 1;
        }
        // Set has to be empty
        assert!(received_ids.is_empty());

        // Verify concurrency
        let max_observed = max_concurrent.load(Ordering::SeqCst);
        assert!(
            max_observed <= MAX_EXECUTORS as usize,
            "Max concurrency {} exceeded limit {}",
            max_observed,
            MAX_EXECUTORS
        );
        tracing::info!(max_observed, "Concurrency observed");
        // Likely even max_observed == 50
        assert!(
            max_observed > 1,
            "Non-blocking messages should execute in parallel"
        );
    }

    #[tokio::test]
    async fn test_mixed_blocking_non_blocking() {
        const NUM_MESSAGES: usize = 100;
        // 30% blocking messages
        const BLOCKING_RATIO: f32 = 0.3;

        let (sender, mut worker, _db) = setup_engine(false);

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        Arc::get_mut(&mut worker.executor_builder)
            .expect("factory not shared before spawn")
            .with_concurrency_tracking(&active_tasks, &max_concurrent);

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Shared key for blocking messages
        let blocking_key =
            pubkey!("1111111111111111111111111111111111111111111");
        // Send mixed messages
        for i in 0..NUM_MESSAGES {
            let is_blocking = rand::random::<f32>() < BLOCKING_RATIO;
            let pubkeys = if is_blocking {
                vec![blocking_key]
            } else {
                vec![Pubkey::new_unique()]
            };

            let msg = create_test_intent(i as u64, &pubkeys, false);
            sender.schedule(vec![msg]).unwrap();
        }

        // Process results
        let mut completed = 0;
        while completed < NUM_MESSAGES {
            let result = result_receiver.recv().await.unwrap();
            assert!(result.is_ok());
            completed += 1;
        }

        // Verify concurrency was between 1 and MAX_CONCURRENCY
        let max_observed = max_concurrent.load(Ordering::SeqCst);
        assert!(
            max_observed >= 1 && max_observed <= MAX_EXECUTORS as usize,
            "Concurrency {} outside expected range",
            max_observed
        );
    }

    // Mock implementations for testing
    #[derive(Clone)]
    enum MockFailureMode {
        None,
        /// Deterministic (non-transient) failure on every attempt
        Persistent,
        /// Transient failure while the counter is above zero
        Transient(Arc<AtomicUsize>),
    }

    pub struct MockIntentExecutorFactory {
        failure_mode: MockFailureMode,
        report_callbacks_on_failure: bool,
        created_instances: Arc<AtomicUsize>,
        active_tasks: Option<Arc<AtomicUsize>>,
        max_concurrent: Option<Arc<AtomicUsize>>,
    }

    impl MockIntentExecutorFactory {
        fn with_mode(failure_mode: MockFailureMode) -> Self {
            Self {
                failure_mode,
                report_callbacks_on_failure: false,
                created_instances: Arc::new(AtomicUsize::new(0)),
                active_tasks: None,
                max_concurrent: None,
            }
        }

        pub fn new() -> Self {
            Self::with_mode(MockFailureMode::None)
        }

        pub fn new_failing() -> Self {
            Self::with_mode(MockFailureMode::Persistent)
        }

        pub fn new_transient(failures: usize) -> Self {
            Self::with_mode(MockFailureMode::Transient(Arc::new(
                AtomicUsize::new(failures),
            )))
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

    impl IntentExecutorBuilder<MockTransactionPreparator>
        for MockIntentExecutorFactory
    {
        fn create_instance(
            &self,
            _status: magicblock_program::outbox_intent_bundles::OutboxIntentBundleStatus,
        ) -> Box<dyn IntentExecutor<MockTransactionPreparator>> {
            self.created_instances.fetch_add(1, Ordering::SeqCst);
            Box::new(MockIntentExecutor {
                failure_mode: self.failure_mode.clone(),
                report_callbacks_on_failure: self.report_callbacks_on_failure,
                active_tasks: self.active_tasks.clone(),
                max_concurrent: self.max_concurrent.clone(),
            })
        }
    }

    pub struct MockIntentExecutor {
        failure_mode: MockFailureMode,
        report_callbacks_on_failure: bool,
        active_tasks: Option<Arc<AtomicUsize>>,
        max_concurrent: Option<Arc<AtomicUsize>>,
    }

    impl MockIntentExecutor {
        fn on_task_started(&self) {
            if let (Some(active), Some(max)) =
                (&self.active_tasks, &self.max_concurrent)
            {
                // Increment active task count
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;

                // Update max concurrent if needed
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
    impl IntentExecutor<MockTransactionPreparator> for MockIntentExecutor {
        async fn execute(
            self: Box<Self>,
            _base_intent: magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle,
        ) -> (
            IntentExecutionResult,
            CleanupHandle<MockTransactionPreparator>,
        ) {
            self.on_task_started();

            // Simulate some work
            sleep(Duration::from_millis(50)).await;

            let success = IntentExecutionResult {
                inner: Ok(ExecutionOutput::TwoStage {
                    commit_signature: Signature::default(),
                    finalize_signature: Signature::default(),
                }),
                patched_errors: vec![],
                callbacks_report: vec![],
                #[cfg(feature = "dev-context-only-utils")]
                successful_transaction_strategies: vec![],
            };
            let result = match &self.failure_mode {
                MockFailureMode::None => success,
                MockFailureMode::Persistent => IntentExecutionResult {
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
                    #[cfg(feature = "dev-context-only-utils")]
                    successful_transaction_strategies: vec![],
                },
                MockFailureMode::Transient(remaining) => {
                    let should_fail = remaining
                        .fetch_update(
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                            |value| value.checked_sub(1),
                        )
                        .is_ok();
                    if should_fail {
                        let callbacks_report =
                            if self.report_callbacks_on_failure {
                                vec![Ok(Signature::default())]
                            } else {
                                vec![]
                            };
                        IntentExecutionResult {
                            inner: Err(ExecutorError::FailedToCommitError {
                                err: TransactionStrategyExecutionError::InternalError(
                                    InternalError::from(
                                        MagicBlockRpcClientError::SentTransactionError(
                                            TransactionError::BlockhashNotFound,
                                            Signature::default(),
                                        ),
                                    ),
                                ),
                                signature: None,
                            }),
                            patched_errors: vec![],
                            callbacks_report,
                            #[cfg(feature = "dev-context-only-utils")]
                            successful_transaction_strategies: vec![],
                        }
                    } else {
                        success
                    }
                }
            };

            self.on_task_finished();

            let cleanup = CleanupHandle::new(
                Keypair::new(),
                vec![],
                false,
                MockTransactionPreparator,
            );
            (result, cleanup)
        }
    }
}
