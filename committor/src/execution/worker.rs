#[cfg(test)]
use crate::execution::{
    error::IntentExecutorError, progress::ExecutionProgress,
    transaction::error::TransactionExecutionError,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[cfg(test)]
use tokio::sync::broadcast;

use metrics::metrics;
use parking_lot::Mutex;
use program::outbox_intent_bundles::OutboxIntentBundle;
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
    time::sleep,
};
use tokio_stream::StreamExt;
use tracing::{error, info, instrument, trace, warn};

use crate::execution::{
    ExecutionOutput, IntentExecutionResult, IntentRunner, error::IntentExecutorResult,
    queue::IntentScheduleError, scheduler::IntentScheduler, stream::IntentStream,
};

const SEMAPHORE_CLOSED_MSG: &str = "Executors semaphore closed!";
/// Max number of executors that can send messages in parallel to Base layer
const MAX_EXECUTORS: u8 = 50;
/// Normal blocked-intent limit, with extra admission while executors are free.
const SCHEDULER_CAPACITY: usize = 3_000;
/// Stop admitting new intents at this limit even when executors are free.
const SCHEDULER_HARD_CAPACITY: usize = 6_000;
/// Yield between batches, including when the in-memory backlog is always ready.
const SCHEDULING_BATCH_SIZE: usize = 64;
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
    capacity_changed: Arc<Notify>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct RecordedResult {
    inner: Result<ExecutionOutput, Arc<IntentExecutorError>>,
    id: u64,
}

#[cfg(test)]
impl RecordedResult {
    fn new(id: u64, result: IntentExecutionResult) -> Self {
        Self {
            id,
            inner: result.inner.map_err(Arc::new),
        }
    }
    fn poisoned(id: u64) -> Self {
        Self {
            id,
            inner: Err(Arc::new(IntentExecutorError::PoisonedIntentError)),
        }
    }
}

#[cfg(test)]
impl std::ops::Deref for RecordedResult {
    type Target = Result<ExecutionOutput, Arc<IntentExecutorError>>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

pub(crate) struct IntentExecutor {
    intent_stream: IntentStream,
    runner: Arc<dyn IntentRunner>,

    scheduler: Arc<Mutex<IntentScheduler>>,
    running_executors: tokio::task::JoinSet<()>,
    executors_semaphore: Arc<Semaphore>,
    retries_semaphore: Arc<Semaphore>,
    execution_capacity_changed: Arc<Notify>,
    scheduler_needs_drain: bool,
    #[cfg(test)]
    result_sender: broadcast::Sender<RecordedResult>,
    stop: tokio_util::sync::CancellationToken,
    tasks: tokio_util::task::TaskTracker,
}

impl IntentExecutor {
    pub(crate) fn new(
        intent_stream: IntentStream,
        runner: Arc<dyn IntentRunner>,
        tasks: tokio_util::task::TaskTracker,
    ) -> Self {
        Self {
            intent_stream,
            tasks,
            runner,
            running_executors: tokio::task::JoinSet::new(),
            executors_semaphore: Arc::new(Semaphore::new(MAX_EXECUTORS as usize)),
            retries_semaphore: Arc::new(Semaphore::new(MAX_SLEEPING_RETRIERS)),
            scheduler: Arc::new(Mutex::new(IntentScheduler::new())),
            execution_capacity_changed: Arc::new(Notify::new()),
            scheduler_needs_drain: false,
            #[cfg(test)]
            result_sender: broadcast::channel(100).0,
            stop: tokio_util::sync::CancellationToken::new(),
        }
    }

    pub(crate) async fn run(
        mut self,
        stop: tokio_util::sync::CancellationToken,
    ) -> Result<(), tokio::task::JoinError> {
        self.stop = stop.clone();
        tokio::select! {
            biased;
            _ = stop.cancelled() => {},
            result = self.main_loop() => result?,
        }
        while let Some(result) = self.running_executors.join_next().await {
            result?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn spawn(self) -> broadcast::Sender<RecordedResult> {
        let sender = self.result_sender.clone();
        tokio::spawn(self.run(tokio_util::sync::CancellationToken::new()));
        sender
    }

    /// Main loop that:
    /// 1. Handles & schedules incoming intents
    /// 2. Finds available executor
    /// 3. Spawns execution of scheduled intent
    #[instrument(skip(self))]
    async fn main_loop(&mut self) -> Result<(), tokio::task::JoinError> {
        let mut iterations = 0;
        loop {
            if iterations == SCHEDULING_BATCH_SIZE {
                tokio::task::yield_now().await;
                iterations = 0;
            }
            iterations += 1;

            let intent = match self.next_scheduled_intent().await {
                Ok(value) => value,
                Err(IntentScheduleError::ChannelClosed) => {
                    info!("Channel closed, exiting");
                    break;
                }
                Err(IntentScheduleError::Worker(err)) => return Err(err),
                Err(IntentScheduleError::DBError(err)) => {
                    // TODO(edwin): add to alert as this is critical error
                    error!(error = ?err, "Failed to fetch intent");
                    continue;
                }
            };
            let Some(intent) = intent else {
                // An intent was blocked, or an execution event requires us to
                // reconsider scheduler readiness and admission capacity.
                trace!("Scheduling event did not select an intent");
                continue;
            };

            // Waiting until there's available executor
            let Ok(permit) = self.executors_semaphore.clone().acquire_owned().await else {
                error!(SEMAPHORE_CLOSED_MSG);
                break;
            };

            // Spawn executor
            let runner = self.runner.clone();
            let scheduler = self.scheduler.clone();
            let limits = ExecutionLimits {
                executors: self.executors_semaphore.clone(),
                retries: self.retries_semaphore.clone(),
                capacity_changed: self.execution_capacity_changed.clone(),
            };

            self.running_executors.spawn(self.tasks.track_future(Self::execute(
                runner,
                intent,
                scheduler,
                limits,
                permit,
                self.stop.clone(),
                #[cfg(test)]
                self.result_sender.clone(),
            )));

            metrics::set_committor_executors_busy_count(self.running_executors.len() as i64);
        }
        Ok(())
    }

    /// Returns [`OutboxIntentBundle`] or None if all intents are blocked
    #[instrument(skip(self))]
    async fn next_scheduled_intent(
        &mut self,
    ) -> Result<Option<OutboxIntentBundle>, IntentScheduleError> {
        let blocked = self.scheduler.lock().intents_blocked();
        let can_receive = blocked < SCHEDULER_CAPACITY
            || (blocked < SCHEDULER_HARD_CAPACITY
                && self.executors_semaphore.available_permits() > 0);
        if !can_receive {
            warn!(blocked_count = blocked, "Scheduler admission paused");
        }

        let running_executors = &mut self.running_executors;
        let intent_stream = &mut self.intent_stream;
        let intent = tokio::select! {
            // Reap completions and drain eligible work before admitting new work.
            biased;
            Some(result) = running_executors.join_next() => {
                result?;
                self.scheduler_needs_drain = true;
                None
            },
            intent = async {
                self.scheduler.lock().pop_next_scheduled_intent()
            }, if self.scheduler_needs_drain => {
                if intent.is_none() {
                    self.scheduler_needs_drain = false;
                }
                intent
            },
            _ = self.execution_capacity_changed.notified() => None,
            result = intent_stream.next(), if can_receive => {
                let intent = result
                    .ok_or(IntentScheduleError::ChannelClosed)?
                    .map_err(IntentScheduleError::DBError)?;
                self.scheduler.lock().schedule(intent)
            },
        };

        Ok(intent)
    }

    /// Completes one intent and releases its execution permit.
    /// Transient failures are retried with a fresh attempt while the scheduler
    /// keeps conflicting intents blocked, preserving per-account commit order.
    #[instrument(skip_all, fields(intent_id = intent.intent_id))]
    async fn execute(
        runner: Arc<dyn IntentRunner>,
        intent: OutboxIntentBundle,
        scheduler: Arc<Mutex<IntentScheduler>>,
        limits: ExecutionLimits,
        execution_permit: OwnedSemaphorePermit,
        stop: tokio_util::sync::CancellationToken,
        #[cfg(test)] result_sender: broadcast::Sender<RecordedResult>,
    ) {
        let instant = Instant::now();

        let Some((mut result, execution_permit)) =
            Self::execute_with_retries(runner, &intent, limits, execution_permit, stop).await
        else {
            return;
        };

        // Report
        let is_err = result.inner.as_ref().inspect_err(|err| {
            error!(intent_id = intent.intent_id, error = ?err, "Failed to execute intent bundle");
        }).is_err();
        Self::execution_metrics(instant.elapsed(), &intent, &result.inner);
        let mut scheduler = scheduler.lock();
        if is_err {
            // Poison this intent's pubkeys and evict any successor that's
            // reachable from them, so a terminal failure can't leave the
            // scheduler permanently stuck on dead accounts.
            let pubkeys = intent.get_all_committed_pubkeys();
            warn!(pubkeys = ?pubkeys, "Intents for following pubkeys are now permanently poisoned for the life of this process.");
            let poisoned_intents = match scheduler.failed(&intent) {
                Ok(intents) => intents,
                Err(err) => {
                    error!(intent_id = intent.intent_id, error = ?err, "Intent scheduler state is inconsistent");
                    result.inner = Err(err.into());
                    Vec::new()
                }
            };
            metrics::set_committor_poisoned_keys_count(scheduler.poisoned_keys_count() as i64);
            drop(scheduler);
            #[cfg(test)]
            Self::broadcast_result(
                RecordedResult::new(intent.intent_id, result),
                &result_sender,
            );
            Self::report_poisoned_intents(
                poisoned_intents,
                #[cfg(test)]
                &result_sender,
            );
        } else {
            // Remove executed task from Scheduler to unblock other intents
            if let Err(err) = scheduler.complete(&intent) {
                error!(intent_id = intent.intent_id, error = ?err, "Intent scheduler state is inconsistent");
                result.inner = Err(err.into());
            }
            drop(scheduler);
            #[cfg(test)]
            Self::broadcast_result(
                RecordedResult::new(intent.intent_id, result),
                &result_sender,
            );
        }

        // Free worker
        drop(execution_permit);
    }

    #[cfg(test)]
    fn broadcast_result(
        broadcasted_result: RecordedResult,
        result_sender: &broadcast::Sender<RecordedResult>,
    ) {
        if result_sender.receiver_count() != 0
            && let Err(err) = result_sender.send(broadcasted_result)
        {
            warn!(error = ?err, "No result listeners");
        };
    }

    fn report_poisoned_intents(
        poisoned_intents: Vec<OutboxIntentBundle>,
        #[cfg(test)] result_sender: &broadcast::Sender<RecordedResult>,
    ) {
        metrics::inc_committor_cascade_voided_intents_count_by(poisoned_intents.len() as u64);
        for intent in poisoned_intents {
            warn!(poisoned_intent = ?intent.intent_id, "Intent poisoned");
            #[cfg(test)]
            Self::broadcast_result(RecordedResult::poisoned(intent.intent_id), result_sender);
        }
    }

    /// Executes an intent, retrying plausibly-transient failures with a
    /// fresh attempt. Returns the final result together with whichever
    /// execution permit is still held (`None` only if the executors
    /// semaphore closed mid-retry).
    async fn execute_with_retries(
        runner: Arc<dyn IntentRunner>,
        intent: &OutboxIntentBundle,
        limits: ExecutionLimits,
        execution_permit: OwnedSemaphorePermit,
        stop: tokio_util::sync::CancellationToken,
    ) -> Option<(IntentExecutionResult, Option<OwnedSemaphorePermit>)> {
        // Commit tasks give on-chain dedup (commit nonce) to re-executed
        // sends; action-only intents can double-execute if their transaction
        // landed unobserved, so they only retry pre-send failures
        let has_dedup_guard = !intent.get_all_committed_pubkeys().is_empty();

        let mut attempt = 0;
        let mut execution_permit = Some(execution_permit);
        let mut current_intent = intent.clone();
        let result = loop {
            if stop.is_cancelled() {
                return None;
            }
            attempt += 1;
            if attempt > 1 {
                current_intent = runner.reconcile_intent(&current_intent).await;
            }

            if stop.is_cancelled() {
                return None;
            }
            let result = runner.clone().execute(&current_intent).await;

            // break early if we can't retry anymore
            if attempt >= MAX_INTENT_ATTEMPTS || !result.is_retriable(has_dedup_guard) {
                break result;
            }

            // Sleeping retries release their executor slot but must stay
            // bounded; without a free retry slot the failure is terminal
            let Ok(retry_permit) = limits.retries.clone().try_acquire_owned() else {
                warn!(intent_id = intent.intent_id, "Retry capacity exhausted");
                break result;
            };

            if let Err(err) = &result.inner {
                warn!(intent_id = intent.intent_id, attempt, error = ?err, "Transient intent failure, retrying");
            }

            // Release the executor slot during backoff so unrelated intents
            // keep executing while this one waits out the outage.
            // Per-intent jitter decorrelates synchronized retry bursts when
            // many intents fail together during an outage
            let jitter = Duration::from_millis((intent.intent_id % 8) * 125);
            drop(execution_permit.take());
            // notify_one retains a notification if the engine is not yet
            // waiting, so a release around guard evaluation cannot be missed.
            limits.capacity_changed.notify_one();
            tokio::select! {
                biased;
                _ = stop.cancelled() => return None,
                _ = sleep(INTENT_RETRY_BACKOFF * attempt + jitter) => {},
            }
            execution_permit = match tokio::select! {
                biased;
                _ = stop.cancelled() => return None,
                permit = limits.executors.clone().acquire_owned() => permit,
            } {
                Ok(permit) => Some(permit),
                Err(_) => {
                    error!(SEMAPHORE_CLOSED_MSG);
                    break result;
                }
            };
            drop(retry_permit);
        };

        Some((result, execution_permit))
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
        if let Err(err) = &result {
            metrics::inc_committor_failed_intents_count(&INTENT_BUNDLE_LABEL, err);
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

#[cfg(test)]
mod admission_tests;

/// Worker tests
#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use rpc_client::MagicBlockRpcClientError;

    use solana_pubkey::{Pubkey, pubkey};
    use solana_signature::Signature;
    use solana_signer::SignerError;
    use solana_transaction_error::TransactionError;
    use tokio::{
        sync::mpsc::{self, Sender},
        time::{sleep, timeout},
    };

    use super::*;
    use crate::{
        execution::{
            IntentExecutionResult, IntentExecutor, IntentRunner,
            backlog::{BacklogDB, DummyDB},
            error::{IntentExecutorError as ExecutorError, InternalError},
            scheduler::create_test_intent_bundle,
            stream::IntentStream,
        },
        test_utils,
    };

    fn setup_engine(
        should_fail: bool,
    ) -> (
        Sender<OutboxIntentBundle>,
        IntentExecutor,
        Arc<Mutex<DummyDB>>,
    ) {
        let runner = if !should_fail { MockRunner::new() } else { MockRunner::new_failing() };
        setup_engine_with_runner(runner)
    }

    fn setup_engine_with_runner(
        runner: MockRunner,
    ) -> (
        Sender<OutboxIntentBundle>,
        IntentExecutor,
        Arc<Mutex<DummyDB>>,
    ) {
        test_utils::init_test_logger();

        let db = Arc::new(Mutex::new(DummyDB::new()));
        let (handle, receiver) = mpsc::channel(1000);
        let intent_stream = IntentStream::new(db.clone(), receiver);
        let worker = IntentExecutor::new(
            intent_stream,
            Arc::new(runner),
            tokio_util::task::TaskTracker::new(),
        );

        (handle, worker, db)
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_new_intents_and_retries() {
        for transient_failure in [false, true] {
            let runner = if transient_failure {
                MockRunner::new_transient(usize::MAX)
            } else {
                MockRunner::new()
            };
            let attempts = runner.attempts.clone();
            let (sender, worker, _) = setup_engine_with_runner(runner);
            let stop = tokio_util::sync::CancellationToken::new();
            let task = tokio::spawn(worker.run(stop.clone()));
            sender
                .send(create_test_intent_bundle(1, &[Pubkey::new_unique()], &[]))
                .await
                .unwrap();
            while attempts.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
            stop.cancel();
            sender
                .send(create_test_intent_bundle(2, &[Pubkey::new_unique()], &[]))
                .await
                .unwrap();
            task.await.unwrap().unwrap();
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                1,
                "shutdown neither starts queued work nor retries the active intent"
            );
        }
    }

    #[tokio::test]
    async fn test_worker_processes_messages() {
        let (sender, worker, _db) = setup_engine(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a test message
        let msg = create_test_intent_bundle(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            &[],
        );
        sender.try_send(msg.clone()).unwrap();

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
        let msg1 = create_test_intent_bundle(1, &[pubkey], &[]);
        let msg2 = create_test_intent_bundle(2, &[pubkey], &[]);

        sender.try_send(msg1.clone()).unwrap();
        sender.try_send(msg2.clone()).unwrap();

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
        let msg2 = create_test_intent_bundle(2, &[a], &[]);

        sender.try_send(msg1.clone()).unwrap();
        sender.try_send(msg2.clone()).unwrap();

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
        let msg = create_test_intent_bundle(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            &[],
        );
        sender.try_send(msg.clone()).unwrap();

        // Verify the failure was properly reported
        let result = result_receiver.recv().await.unwrap();
        assert!(result.inner.is_err());
        assert_eq!(result.id, 1);
        assert_eq!(
            result.inner.unwrap_err().to_string(),
            "Transaction execution failed: InternalError: SignerError: custom error: oops"
        );
    }

    #[tokio::test]
    async fn test_worker_falls_back_to_db_when_channel_empty() {
        let (_sender, worker, db) = setup_engine(false);

        // Add a message to the DB
        let msg = create_test_intent_bundle(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            &[],
        );
        db.lock().store_intent_bundles(vec![msg.clone()]).unwrap();

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

        let mut factory = MockRunner::new();

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        factory.with_concurrency_tracking(&active_tasks, &max_concurrent);
        let (sender, worker, _db) = setup_engine_with_runner(factory);

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a flood of messages
        for i in 0..NUM_MESSAGES {
            let msg = create_test_intent_bundle(
                i as u64,
                &[pubkey!("1111111111111111111111111111111111111111111")],
                &[],
            );
            sender.try_send(msg).unwrap();
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

    /// A failed head reports its own real error, and every successor
    /// voided by the poisoning cascade gets its own `PoisonedIntentError`
    /// report - not silence. Previously the cascaded successors were left
    /// in `blocked_intents` forever with no report at all, which hung this
    /// exact test (10 intents sharing one pubkey, waiting for 10 results)
    /// indefinitely. The bounded `timeout` here turns any regression back
    /// into a fast failure instead of another indefinite hang.
    #[tokio::test]
    async fn test_multiple_failures() {
        let (sender, worker, _db) = setup_engine(true); // Worker that always fails
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // All intents share one pubkey: the head executes and fails, and
        // the cascade voids every successor queued behind it.
        const NUM_FAILURES: usize = 10;
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        for i in 0..NUM_FAILURES {
            let msg = create_test_intent_bundle(i as u64, &[pubkey], &[]);
            sender.try_send(msg).unwrap();
        }

        let mut results = Vec::with_capacity(NUM_FAILURES);
        for _ in 0..NUM_FAILURES {
            let result = timeout(Duration::from_secs(5), result_receiver.recv())
                .await
                .expect("must not hang waiting for poisoned successors to be reported")
                .unwrap();
            results.push(result);
        }

        // The head (id 0) executed and reports its own real error.
        let head = results.iter().find(|r| r.id == 0).unwrap();
        assert!(
            matches!(
                head.inner.as_ref().unwrap_err().as_ref(),
                ExecutorError::FailedExecution(_)
            ),
            "head should report the real executor error, got {:?}",
            head.inner
        );

        // Every other intent was queued behind the head, never executed,
        // and was voided by the cascade instead.
        for id in 1..NUM_FAILURES as u64 {
            let voided = results.iter().find(|r| r.id == id).unwrap();
            assert!(
                matches!(
                    voided.inner.as_ref().unwrap_err().as_ref(),
                    ExecutorError::PoisonedIntentError
                ),
                "intent {id} should be reported as poisoned, got {:?}",
                voided.inner
            );
        }
    }

    /// A poisoned pubkey rejects future intents silently (no broadcast -
    /// they never entered the scheduler), while unrelated pubkeys keep
    /// executing and reporting normally, proving poisoning doesn't leak
    /// across unrelated accounts.
    #[tokio::test]
    async fn test_poisoned_pubkey_rejects_future_intents_silently() {
        let (sender, worker, _db) = setup_engine(true); // Worker that always fails
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let poisoned_pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let head = create_test_intent_bundle(0, &[poisoned_pubkey], &[]);
        let successor = create_test_intent_bundle(1, &[poisoned_pubkey], &[]);
        sender.try_send(head).unwrap();
        sender.try_send(successor).unwrap();

        // Head fails for real, successor is voided by the cascade.
        for _ in 0..2 {
            let result = timeout(Duration::from_secs(5), result_receiver.recv())
                .await
                .expect("must not hang")
                .unwrap();
            assert!(result.is_err());
        }

        // A brand new intent on the now-poisoned pubkey is rejected at
        // admission - it never reaches an executor, so no broadcast for
        // it ever arrives.
        let rejected = create_test_intent_bundle(2, &[poisoned_pubkey], &[]);
        sender.try_send(rejected).unwrap();
        let silence = timeout(Duration::from_millis(300), result_receiver.recv()).await;
        assert!(
            silence.is_err(),
            "poisoned pubkey must not produce a broadcast for a rejected intent"
        );

        // An intent on an unrelated pubkey is unaffected by the poisoning
        // and still executes (and fails for real, not as poisoned).
        let unrelated_pubkey = pubkey!("21111111111111111111111111111111111111111111");
        let unrelated = create_test_intent_bundle(3, &[unrelated_pubkey], &[]);
        sender.try_send(unrelated).unwrap();
        let result = timeout(Duration::from_secs(5), result_receiver.recv())
            .await
            .expect("must not hang")
            .unwrap();
        assert_eq!(result.id, 3);
        assert!(
            matches!(
                result.inner.as_ref().unwrap_err().as_ref(),
                ExecutorError::FailedExecution(_)
            ),
            "unrelated pubkey should fail for real, not as poisoned: {:?}",
            result.inner
        );
    }

    /// Transient failures are retried with a fresh attempt until success
    #[tokio::test(start_paused = true)]
    async fn test_transient_failure_retried_until_success() {
        let factory = MockRunner::new_transient(2);
        let attempts = factory.attempts.clone();
        let (sender, worker, _db) = setup_engine_with_runner(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent_bundle(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            &[],
        );
        sender.try_send(msg).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    /// Transient failures stop being retried once attempts are exhausted
    #[tokio::test(start_paused = true)]
    async fn test_transient_failure_exhausts_attempts() {
        let factory = MockRunner::new_transient(usize::MAX);
        let attempts = factory.attempts.clone();
        let (sender, worker, _db) = setup_engine_with_runner(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent_bundle(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            &[],
        );
        sender.try_send(msg).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_err());
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            MAX_INTENT_ATTEMPTS as usize
        );
    }

    /// Non-transient failures are terminal on the first attempt
    #[tokio::test]
    async fn test_non_transient_failure_not_retried() {
        let factory = MockRunner::new_failing();
        let attempts = factory.attempts.clone();
        let (sender, worker, _db) = setup_engine_with_runner(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent_bundle(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            &[],
        );
        sender.try_send(msg).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    /// Once action callbacks fired, even a transient failure must not retry
    #[tokio::test]
    async fn test_transient_failure_with_callbacks_not_retried() {
        let mut factory = MockRunner::new_transient(usize::MAX);
        factory.report_callbacks_on_failure = true;
        let attempts = factory.attempts.clone();
        let (sender, worker, _db) = setup_engine_with_runner(factory);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        let msg = create_test_intent_bundle(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
            &[],
        );
        sender.try_send(msg).unwrap();

        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_non_blocking_messages() {
        const NUM_MESSAGES: u64 = 200;

        let mut factory = MockRunner::new();

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        factory.with_concurrency_tracking(&active_tasks, &max_concurrent);
        let (sender, worker, _db) = setup_engine_with_runner(factory);

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send messages with unique keys (non-blocking)
        let mut received_ids = HashSet::new();
        for i in 0..NUM_MESSAGES {
            let unique_pubkey = Pubkey::new_unique(); // Each message gets unique key
            let msg = create_test_intent_bundle(i, &[unique_pubkey], &[]);

            received_ids.insert(i);
            sender.try_send(msg).unwrap();
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

        let mut factory = MockRunner::new();

        let active_tasks = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        factory.with_concurrency_tracking(&active_tasks, &max_concurrent);
        let (sender, worker, _db) = setup_engine_with_runner(factory);

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Shared key for blocking messages
        let blocking_key = pubkey!("1111111111111111111111111111111111111111111");
        // Send mixed messages
        for i in 0..NUM_MESSAGES {
            let is_blocking = rand::random::<f32>() < BLOCKING_RATIO;
            let pubkeys = if is_blocking { vec![blocking_key] } else { vec![Pubkey::new_unique()] };

            let msg = create_test_intent_bundle(i as u64, &pubkeys, &[]);
            sender.try_send(msg).unwrap();
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

    pub(super) struct MockRunner {
        failure_mode: MockFailureMode,
        report_callbacks_on_failure: bool,
        attempts: Arc<AtomicUsize>,
        active_tasks: Option<Arc<AtomicUsize>>,
        max_concurrent: Option<Arc<AtomicUsize>>,
    }

    impl MockRunner {
        fn with_mode(failure_mode: MockFailureMode) -> Self {
            Self {
                failure_mode,
                report_callbacks_on_failure: false,
                attempts: Arc::new(AtomicUsize::new(0)),
                active_tasks: None,
                max_concurrent: None,
            }
        }

        pub(super) fn new() -> Self {
            Self::with_mode(MockFailureMode::None)
        }

        pub(super) fn new_failing() -> Self {
            Self::with_mode(MockFailureMode::Persistent)
        }

        pub(super) fn new_transient(failures: usize) -> Self {
            Self::with_mode(MockFailureMode::Transient(Arc::new(AtomicUsize::new(
                failures,
            ))))
        }

        pub(super) fn with_concurrency_tracking(
            &mut self,
            active_tasks: &Arc<AtomicUsize>,
            max_concurrent: &Arc<AtomicUsize>,
        ) {
            self.active_tasks = Some(active_tasks.clone());
            self.max_concurrent = Some(max_concurrent.clone());
        }
    }

    impl MockRunner {
        fn on_task_started(&self) {
            if let (Some(active), Some(max)) = (&self.active_tasks, &self.max_concurrent) {
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
    impl IntentRunner for MockRunner {
        async fn execute(self: Arc<Self>, _intent: &OutboxIntentBundle) -> IntentExecutionResult {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            self.on_task_started();

            // Simulate some work
            sleep(Duration::from_millis(50)).await;

            let output = ExecutionOutput {
                confirmed_commit: Some(Signature::default()),
                signature: Signature::default(),
            };
            let success = IntentExecutionResult {
                inner: Ok(output),
                progress: output.progress(),
                callbacks_reported: false,
            };
            let result = match &self.failure_mode {
                MockFailureMode::None => success,
                MockFailureMode::Persistent => IntentExecutionResult {
                    inner: Err(ExecutorError::FailedExecution(
                        TransactionExecutionError::InternalError(InternalError::SignerError(
                            SignerError::Custom("oops".to_string()),
                        )),
                    )),
                    progress: ExecutionProgress::default(),
                    callbacks_reported: false,
                },
                MockFailureMode::Transient(remaining) => {
                    let should_fail = remaining
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                            value.checked_sub(1)
                        })
                        .is_ok();
                    if should_fail {
                        IntentExecutionResult {
                            inner: Err(ExecutorError::FailedExecution(
                                TransactionExecutionError::InternalError(InternalError::from(
                                    MagicBlockRpcClientError::SentTransactionError(
                                        TransactionError::BlockhashNotFound,
                                        Signature::default(),
                                    ),
                                )),
                            )),
                            progress: ExecutionProgress {
                                transaction:
                                    crate::execution::progress::TransactionProgress::Rejected(
                                        Signature::default(),
                                    ),
                                ..Default::default()
                            },
                            callbacks_reported: self.report_callbacks_on_failure,
                        }
                    } else {
                        success
                    }
                }
            };

            self.on_task_finished();

            result
        }
    }
}
