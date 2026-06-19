use std::{
    ops::Deref,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::{stream::FuturesUnordered, StreamExt};
use magicblock_core::traits::CallbackScheduleError;
use magicblock_metrics::metrics;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_signature::Signature;
use tokio::{
    sync::{
        broadcast, mpsc, mpsc::error::TryRecvError, watch,
        OwnedSemaphorePermit, Semaphore,
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, trace, warn};

use crate::{
    intent_execution_manager::{
        db::DB,
        intent_scheduler::{IntentScheduler, POISONED_INNER_MSG},
        IntentExecutionManagerError,
    },
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        intent_executor_factory::IntentExecutorFactory,
        ExecutionOutput, IntentExecutionResult, IntentExecutor,
    },
    persist::IntentPersister,
};

const SEMAPHORE_CLOSED_MSG: &str = "Executors semaphore closed!";
/// Max number of executors that can send messages in parallel to Base layer
const MAX_EXECUTORS: u8 = 50;

pub type PatchedErrors = Vec<TransactionStrategyExecutionError>;

#[derive(Clone, Debug)]
pub struct BroadcastedIntentExecutionResult {
    pub inner: Result<ExecutionOutput, Arc<IntentExecutorError>>,
    pub id: u64,
    pub patched_errors: Arc<PatchedErrors>,
    pub callbacks_report: Vec<Result<Signature, Arc<CallbackScheduleError>>>,
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

pub(crate) struct IntentExecutionEngine<D, P, F> {
    db: Arc<D>,
    executor_factory: F,
    intents_persister: Option<P>,
    receiver: mpsc::Receiver<ScheduledIntentBundle>,

    inner: Arc<Mutex<IntentScheduler>>,
    running_executors: FuturesUnordered<JoinHandle<()>>,
    executors_semaphore: Arc<Semaphore>,
}

impl<D, P, F, E> IntentExecutionEngine<D, P, F>
where
    D: DB,
    P: IntentPersister,
    F: IntentExecutorFactory<Executor = E> + Send + Sync + 'static,
    E: IntentExecutor,
{
    pub fn new(
        db: Arc<D>,
        executor_factory: F,
        intents_persister: Option<P>,
        receiver: mpsc::Receiver<ScheduledIntentBundle>,
    ) -> Self {
        Self {
            db,
            intents_persister,
            executor_factory,
            receiver,
            running_executors: FuturesUnordered::new(),
            executors_semaphore: Arc::new(Semaphore::new(
                MAX_EXECUTORS as usize,
            )),
            inner: Arc::new(Mutex::new(IntentScheduler::new())),
        }
    }

    /// Spawns `main_loop` and return `Receiver` listening to results
    pub fn spawn(
        self,
        shutdown: CancellationToken,
        idle_tx: watch::Sender<bool>,
    ) -> ResultSubscriber {
        let (result_sender, _) = broadcast::channel(100);
        tokio::spawn(self.main_loop(result_sender.clone(), shutdown, idle_tx));

        ResultSubscriber(result_sender)
    }

    /// Main loop that:
    /// 1. Handles & schedules incoming intents
    /// 2. Finds available executor
    /// 3. Spawns execution of scheduled intent
    /// 4. On shutdown, drains queued and in-flight work before exiting
    #[instrument(skip(self, result_sender, shutdown, idle_tx))]
    async fn main_loop(
        mut self,
        result_sender: broadcast::Sender<BroadcastedIntentExecutionResult>,
        shutdown: CancellationToken,
        idle_tx: watch::Sender<bool>,
    ) {
        let mut draining = false;

        loop {
            if !draining && shutdown.is_cancelled() {
                draining = true;
                info!("IntentExecutionEngine entering drain mode");
            }

            if draining && self.is_fully_idle() {
                break;
            }

            let intent = match self.next_scheduled_intent(draining).await {
                Ok(value) => value,
                Err(IntentExecutionManagerError::ChannelClosed) => {
                    if draining && self.is_fully_idle() {
                        break;
                    }
                    if draining {
                        trace!("Intent channel closed while draining, waiting for in-flight work");
                        continue;
                    }
                    info!("Channel closed, exiting");
                    break;
                }
                Err(IntentExecutionManagerError::ShuttingDown) => {
                    unreachable!("engine does not emit ShuttingDown")
                }
                Err(IntentExecutionManagerError::DBError(err)) => {
                    error!(error = ?err, "Failed to fetch intent");
                    break;
                }
            };
            let Some(intent) = intent else {
                // We couldn't pick up intent for execution due to:
                // 1. All executors are currently busy
                // 2. All intents are blocked and none could be executed at the moment
                // 3. Drain mode with no ingress left to pull
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
            let executor = self.executor_factory.create_instance();
            let persister = self.intents_persister.clone();
            let inner = self.inner.clone();

            let handle = tokio::spawn(Self::execute(
                executor,
                persister,
                intent,
                inner,
                permit,
                result_sender.clone(),
            ));

            self.running_executors.push(handle);
            metrics::set_committor_executors_busy_count(
                self.running_executors.len() as i64,
            );
        }

        while let Some(result) = self.running_executors.next().await {
            if let Err(err) = result {
                error!(error = ?err, "Executor failed during shutdown drain");
            }
        }
        metrics::set_committor_executors_busy_count(0);

        if idle_tx.send(true).is_err() {
            trace!("No shutdown observers for intent execution engine");
        }
        info!("IntentExecutionEngine shutdown complete");
    }

    fn has_pending_ingress(&self) -> bool {
        !self.receiver.is_empty() || !self.db.is_empty()
    }

    fn is_fully_idle(&self) -> bool {
        self.running_executors.is_empty()
            && !self.has_pending_ingress()
            && self.inner.lock().expect(POISONED_INNER_MSG).is_idle()
    }

    /// Returns [`ScheduledIntentBundle`] or None if all intents are blocked
    #[instrument(skip(self))]
    async fn next_scheduled_intent(
        &mut self,
        draining: bool,
    ) -> Result<Option<ScheduledIntentBundle>, IntentExecutionManagerError>
    {
        // Limit on number of intents that can be stored in scheduler
        const SCHEDULER_CAPACITY: usize = 1000;

        let can_receive = || {
            if draining {
                return false;
            }
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

        let can_pull_ingress =
            can_receive() || (draining && self.has_pending_ingress());

        let running_executors = &mut self.running_executors;
        let receiver = &mut self.receiver;
        let db = &self.db;
        let intent = tokio::select! {
            // Notify polled first to prioritize unblocked intents over new one
            biased;
            Some(result) = running_executors.next(), if !running_executors.is_empty() => {
                if let Err(err) = result {
                    error!(error = ?err, "Executor failed");
                };
                trace!("Worker executed intent bundle, fetching new available one");
                self.inner.lock().expect(POISONED_INNER_MSG).pop_next_scheduled_intent()
            },
            result = Self::get_new_intent(receiver, db, draining), if can_pull_ingress => {
                match result? {
                    Some(intent) => {
                        self.inner.lock().expect(POISONED_INNER_MSG).schedule(intent)
                    }
                    None => return Ok(None),
                }
            },
            else => {
                if draining {
                    return Ok(None);
                }
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
        receiver: &mut mpsc::Receiver<ScheduledIntentBundle>,
        db: &Arc<D>,
        draining: bool,
    ) -> Result<Option<ScheduledIntentBundle>, IntentExecutionManagerError>
    {
        match receiver.try_recv() {
            Ok(val) => Ok(Some(val)),
            Err(TryRecvError::Empty) => {
                // Worker either cleaned-up congested channel and now need to clean-up DB
                // or we're just waiting on empty channel
                if let Some(intent_bundle) = db.pop_intent_bundle().await? {
                    Ok(Some(intent_bundle))
                } else if draining {
                    Ok(None)
                } else {
                    receiver
                        .recv()
                        .await
                        .map(Some)
                        .ok_or(IntentExecutionManagerError::ChannelClosed)
                }
            }
            Err(TryRecvError::Disconnected) => {
                Err(IntentExecutionManagerError::ChannelClosed)
            }
        }
    }

    /// Wrapper on [`IntentExecutor`] that handles its results and drops execution permit
    #[instrument(skip(executor, persister, intent, inner_scheduler, execution_permit, result_sender), fields(intent_id = intent.id))]
    async fn execute(
        mut executor: E,
        persister: Option<P>,
        intent: ScheduledIntentBundle,
        inner_scheduler: Arc<Mutex<IntentScheduler>>,
        execution_permit: OwnedSemaphorePermit,
        result_sender: broadcast::Sender<BroadcastedIntentExecutionResult>,
    ) {
        let instant = Instant::now();

        // Execute an Intent
        let result = executor.execute(intent.clone(), persister).await;
        let execution_succeeded = result.inner.is_ok();
        let _ = result.inner.as_ref().inspect_err(|err| {
            error!(intent_id = intent.id, error = ?err, "Failed to execute intent bundle");
        });

        // Record metrics after execution
        Self::execution_metrics(instant.elapsed(), &intent, &result.inner);

        // Broadcast result to subscribers
        let broadcasted_result =
            BroadcastedIntentExecutionResult::new(intent.id, result);
        if let Err(err) = result_sender.send(broadcasted_result) {
            warn!(error = ?err, "No result listeners");
        }

        // Remove executed task from Scheduler to unblock other intents
        // SAFETY: Self::execute is called ONLY after IntentScheduler
        // successfully is able to schedule execution of some Intent
        // that means that the same Intent is SAFE to complete
        inner_scheduler
            .lock()
            .expect(POISONED_INNER_MSG)
            .complete(&intent)
            .expect("Valid completion of previously scheduled message");

        // Cleaning buffers on failure isn't safe due to potential race condition:
        // Assume pubkey set A being committed
        // Intent1 fails and cleans up, another Intent2 with set A executes right away
        // That could lead for Intent2 init of buffers executing prior of Intent1 buffer cleanup
        // With same set A buffers will have same address
        //
        // To avoid this race on buffers we cleanup only succesfully executed intents
        // With intent retries all buffers will be eventually closed once intent succeeds
        //
        // Note: not sure this is safa for TableMania as it susceptible to same race condition
        // unless it does handle it internally somehow
        if execution_succeeded {
            tokio::spawn(async move {
                if let Err(err) = executor.cleanup().await {
                    error!(error = ?err, "Failed to cleanup after intent");
                }
            });
        }

        // Free worker
        drop(execution_permit);
    }

    /// Records metrics related to intent execution
    fn execution_metrics(
        execution_time: Duration,
        intent: &ScheduledIntentBundle,
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

    use solana_pubkey::{pubkey, Pubkey};

    use super::*;
    use crate::intent_execution_manager::{
        db::DB,
        intent_scheduler::{create_test_intent, create_test_intent_bundle},
        test_support::{self, setup_engine, spawn_engine, wait_idle},
    };

    fn subscribe(
        worker: test_support::TestEngine,
    ) -> broadcast::Receiver<BroadcastedIntentExecutionResult> {
        spawn_engine(worker).result_subscriber.subscribe()
    }

    #[tokio::test]
    async fn test_worker_processes_messages() {
        let (sender, worker) = setup_engine(false);
        let mut result_receiver = subscribe(worker);

        // Send a test message
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.send(msg.clone()).await.unwrap();

        // Verify the message was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.id, 1);
    }

    #[tokio::test]
    async fn test_worker_handles_conflicting_messages() {
        let (sender, worker) = setup_engine(false);
        let mut result_receiver = subscribe(worker);

        // Send two conflicting messages
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey], false);
        let msg2 = create_test_intent(2, &[pubkey], false);

        sender.send(msg1.clone()).await.unwrap();
        sender.send(msg2.clone()).await.unwrap();

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
        let (sender, worker) = setup_engine(false);
        let mut result_receiver = subscribe(worker);

        // Send two conflicting messages
        let a = pubkey!("1111111111111111111111111111111111111111111");
        let b = pubkey!("21111111111111111111111111111111111111111111");
        let msg1 = create_test_intent_bundle(1, &[a], &[b]);
        let msg2 = create_test_intent(2, &[a], false);

        sender.send(msg1.clone()).await.unwrap();
        sender.send(msg2.clone()).await.unwrap();

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
        let (sender, worker) = setup_engine(true);
        let mut result_receiver = subscribe(worker);

        // Send a test message that will fail
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.send(msg.clone()).await.unwrap();

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
        let (_sender, worker) = setup_engine(false);

        // Add a message to the DB
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        worker.db.store_intent_bundle(msg.clone()).await.unwrap();

        // Start worker
        let mut result_receiver = subscribe(worker);

        // Verify the message from DB was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.id, 1);
    }

    /// Tests multiple blocking messages being sent at the same time
    #[tokio::test]
    async fn test_high_throughput_message_processing() {
        const NUM_MESSAGES: usize = 20;

        let (sender, mut worker) = setup_engine(false);

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        worker
            .executor_factory
            .with_concurrency_tracking(&active_tasks, &max_concurrent);

        let mut result_receiver = subscribe(worker);

        // Send a flood of messages
        for i in 0..NUM_MESSAGES {
            let msg = create_test_intent(
                i as u64,
                &[pubkey!("1111111111111111111111111111111111111111111")],
                false,
            );
            sender.send(msg).await.unwrap();
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
        let (sender, worker) = setup_engine(true); // Worker that always fails
        let mut result_receiver = subscribe(worker);

        // Send several messages that will fail
        const NUM_FAILURES: usize = 10;
        for i in 0..NUM_FAILURES {
            let msg = create_test_intent(
                i as u64,
                &[pubkey!("1111111111111111111111111111111111111111111")],
                false,
            );
            sender.send(msg).await.unwrap();
        }

        // Verify all failures are processed and semaphore slots released
        for _ in 0..NUM_FAILURES {
            let result = result_receiver.recv().await.unwrap();
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn test_non_blocking_messages() {
        const NUM_MESSAGES: u64 = 200;

        let (sender, mut worker) = setup_engine(false);

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        worker
            .executor_factory
            .with_concurrency_tracking(&active_tasks, &max_concurrent);

        let mut result_receiver = subscribe(worker);

        // Send messages with unique keys (non-blocking)
        let mut received_ids = HashSet::new();
        for i in 0..NUM_MESSAGES {
            let unique_pubkey = Pubkey::new_unique(); // Each message gets unique key
            let msg = create_test_intent(i, &[unique_pubkey], false);

            received_ids.insert(i);
            sender.send(msg).await.unwrap();
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

        let (sender, mut worker) = setup_engine(false);

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        worker
            .executor_factory
            .with_concurrency_tracking(&active_tasks, &max_concurrent);

        let mut result_receiver = subscribe(worker);

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
            sender.send(msg).await.unwrap();
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

    #[tokio::test]
    async fn test_shutdown_drains_in_flight_intent() {
        let (sender, worker) = test_support::setup_engine_with_factory(
            test_support::MockIntentExecutorFactory::new()
                .with_execution_delay(Duration::from_millis(200)),
        );
        let spawned = spawn_engine(worker);
        let mut result_receiver = spawned.result_subscriber.subscribe();

        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        sender.send(msg).await.unwrap();
        spawned.shutdown.cancel();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            result_receiver.recv(),
        )
        .await
        .expect("timed out waiting for in-flight intent result")
        .expect("result channel closed");
        assert!(result.is_ok());
        assert_eq!(result.id, 1);

        let mut idle_rx = spawned.idle_rx.clone();
        wait_idle(&mut idle_rx).await;
    }

    #[tokio::test]
    async fn test_shutdown_drains_conflicting_intents() {
        let (sender, worker) = setup_engine(false);
        let spawned = spawn_engine(worker);
        let mut result_receiver = spawned.result_subscriber.subscribe();

        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        sender
            .send(create_test_intent(1, &[pubkey], false))
            .await
            .unwrap();
        sender
            .send(create_test_intent(2, &[pubkey], false))
            .await
            .unwrap();
        spawned.shutdown.cancel();

        let result1 = tokio::time::timeout(
            Duration::from_secs(5),
            result_receiver.recv(),
        )
        .await
        .expect("timed out waiting for first intent result")
        .expect("result channel closed");
        assert!(result1.is_ok());
        assert_eq!(result1.id, 1);

        let result2 = tokio::time::timeout(
            Duration::from_secs(5),
            result_receiver.recv(),
        )
        .await
        .expect("timed out waiting for second intent result")
        .expect("result channel closed");
        assert!(result2.is_ok());
        assert_eq!(result2.id, 2);

        let mut idle_rx = spawned.idle_rx.clone();
        wait_idle(&mut idle_rx).await;
    }

    #[tokio::test]
    async fn test_shutdown_drains_db_backlog() {
        let (_sender, worker) = setup_engine(false);
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            false,
        );
        worker.db.store_intent_bundle(msg).await.unwrap();

        let spawned = spawn_engine(worker);
        let mut result_receiver = spawned.result_subscriber.subscribe();
        spawned.shutdown.cancel();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            result_receiver.recv(),
        )
        .await
        .expect("timed out waiting for db backlog intent result")
        .expect("result channel closed");
        assert!(result.is_ok());
        assert_eq!(result.id, 1);

        let mut idle_rx = spawned.idle_rx.clone();
        wait_idle(&mut idle_rx).await;
    }

    #[tokio::test]
    async fn test_shutdown_drains_queued_channel_intents() {
        let (sender, worker) = setup_engine(false);
        let spawned = spawn_engine(worker);
        let mut result_receiver = spawned.result_subscriber.subscribe();

        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        for id in 1..=3 {
            sender
                .send(create_test_intent(id, &[pubkey], false))
                .await
                .unwrap();
        }
        spawned.shutdown.cancel();

        for expected_id in 1..=3 {
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                result_receiver.recv(),
            )
            .await
            .expect("timed out waiting for queued intent result")
            .expect("result channel closed");
            assert!(result.is_ok());
            assert_eq!(result.id, expected_id);
        }

        let mut idle_rx = spawned.idle_rx.clone();
        wait_idle(&mut idle_rx).await;
    }
}
