use std::sync::{Arc, Mutex};

use futures_util::{stream::FuturesUnordered, StreamExt};
use log::{error, info, trace, warn};
use magicblock_metrics::metrics;
use tokio::{
    sync::{
        broadcast, mpsc, mpsc::error::TryRecvError, OwnedSemaphorePermit,
        Semaphore,
    },
    task::JoinHandle,
};

use crate::{
    intent_execution_manager::{
        db::DB,
        intent_scheduler::{IntentScheduler, POISONED_INNER_MSG},
        IntentExecutionManagerError,
    },
    intent_executor::{
        error::{IntentExecutorError, IntentExecutorResult},
        intent_executor_factory::IntentExecutorFactory,
        ExecutionOutput, IntentExecutor,
    },
    persist::IntentPersister,
    types::{ScheduledBaseIntentWrapper, TriggerType},
};

const SEMAPHORE_CLOSED_MSG: &str = "Executors semaphore closed!";
/// Max number of executors that can send messages in parallel to Base layer
const MAX_EXECUTORS: u8 = 50;

#[derive(Clone, Debug)]
pub struct ExecutionOutputWrapper {
    pub id: u64,
    pub output: ExecutionOutput,
    pub trigger_type: TriggerType,
}

pub type BroadcastedError = (u64, TriggerType, Arc<IntentExecutorError>);

pub type BroadcastedIntentExecutionResult =
    IntentExecutorResult<ExecutionOutputWrapper, BroadcastedError>;

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
    receiver: mpsc::Receiver<ScheduledBaseIntentWrapper>,

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
        receiver: mpsc::Receiver<ScheduledBaseIntentWrapper>,
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
    pub fn spawn(self) -> ResultSubscriber {
        let (result_sender, _) = broadcast::channel(100);
        tokio::spawn(self.main_loop(result_sender.clone()));

        ResultSubscriber(result_sender)
    }

    /// Main loop that:
    /// 1. Handles & schedules incoming intents
    /// 2. Finds available executor
    /// 3. Spawns execution of scheduled intent
    async fn main_loop(
        mut self,
        result_sender: broadcast::Sender<BroadcastedIntentExecutionResult>,
    ) {
        loop {
            let intent = match self.next_scheduled_intent().await {
                Ok(value) => value,
                Err(IntentExecutionManagerError::ChannelClosed) => {
                    info!("Channel closed, exiting IntentExecutionEngine::main_loop");
                    break;
                }
                Err(IntentExecutionManagerError::DBError(err)) => {
                    error!("Failed to fetch intent from db: {:?}", err);
                    break;
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
    }

    /// Returns [`ScheduledBaseIntentWrapper`] or None if all intents are blocked
    async fn next_scheduled_intent(
        &mut self,
    ) -> Result<Option<ScheduledBaseIntentWrapper>, IntentExecutionManagerError>
    {
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
                warn!("Scheduler capacity exceeded: {}", num_blocked_intents);
                false
            }
        };

        let running_executors = &mut self.running_executors;
        let receiver = &mut self.receiver;
        let db = &self.db;
        let intent = tokio::select! {
            // Notify polled first to prioritize unblocked intents over new one
            biased;
            Some(result) = running_executors.next() => {
                if let Err(err) = result {
                    error!("Executor failed to complete: {}", err);
                };
                trace!("Worker executed BaseIntent, fetching new available one");
                self.inner.lock().expect(POISONED_INNER_MSG).pop_next_scheduled_intent()
            },
            result = Self::get_new_intent(receiver, db), if can_receive() => {
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

    /// Returns [`ScheduledBaseIntentWrapper`] from external channel
    async fn get_new_intent(
        receiver: &mut mpsc::Receiver<ScheduledBaseIntentWrapper>,
        db: &Arc<D>,
    ) -> Result<ScheduledBaseIntentWrapper, IntentExecutionManagerError> {
        match receiver.try_recv() {
            Ok(val) => Ok(val),
            Err(TryRecvError::Empty) => {
                // Worker either cleaned-up congested channel and now need to clean-up DB
                // or we're just waiting on empty channel
                if let Some(base_intent) = db.pop_base_intent().await? {
                    Ok(base_intent)
                } else {
                    receiver
                        .recv()
                        .await
                        .ok_or(IntentExecutionManagerError::ChannelClosed)
                }
            }
            Err(TryRecvError::Disconnected) => {
                Err(IntentExecutionManagerError::ChannelClosed)
            }
        }
    }

    /// Wrapper on [`IntentExecutor`] that handles its results and drops execution permit
    async fn execute(
        executor: E,
        persister: Option<P>,
        intent: ScheduledBaseIntentWrapper,
        inner_scheduler: Arc<Mutex<IntentScheduler>>,
        execution_permit: OwnedSemaphorePermit,
        result_sender: broadcast::Sender<BroadcastedIntentExecutionResult>,
    ) {
        let result = executor
            .execute(intent.inner.clone(), persister)
            .await
            .inspect_err(|err| {
                error!(
                    "Failed to execute BaseIntent. id: {}. {:?}",
                    intent.id, err
                )
            })
            .map(|output| ExecutionOutputWrapper {
                id: intent.id,
                trigger_type: intent.trigger_type,
                output,
            })
            .map_err(|err| {
                // Increase failed intents metric as well
                metrics::inc_committor_failed_intents_count();
                (intent.inner.id, intent.trigger_type, Arc::new(err))
            });

        // Broadcast result to subscribers
        if let Err(err) = result_sender.send(result) {
            warn!("No result listeners of intent execution: {}", err);
        }

        // Remove executed task from Scheduler to unblock other intents
        // SAFETY: Self::execute is called ONLY after IntentScheduler
        // successfully is able to schedule execution of some Intent
        // that means that the same Intent is SAFE to complete
        inner_scheduler
            .lock()
            .expect(POISONED_INNER_MSG)
            .complete(&intent.inner)
            .expect("Valid completion of previously scheduled message");

        // Free worker
        drop(execution_permit);
    }
}

/// Worker tests
#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use magicblock_program::magic_scheduled_base_intent::ScheduledBaseIntent;
    use solana_pubkey::{pubkey, Pubkey};
    use solana_sdk::{signature::Signature, signer::SignerError};
    use tokio::{sync::mpsc, time::sleep};

    use super::*;
    use crate::{
        intent_execution_manager::{
            db::{DummyDB, DB},
            intent_scheduler::create_test_intent,
        },
        intent_executor::{
            error::{
                IntentExecutorError as ExecutorError, IntentExecutorResult,
                InternalError,
            },
            task_info_fetcher::{
                ResetType, TaskInfoFetcher, TaskInfoFetcherResult,
            },
        },
        persist::IntentPersisterImpl,
    };

    type MockIntentExecutionEngine = IntentExecutionEngine<
        DummyDB,
        IntentPersisterImpl,
        MockIntentExecutorFactory,
    >;
    fn setup_engine(
        should_fail: bool,
    ) -> (
        mpsc::Sender<ScheduledBaseIntentWrapper>,
        MockIntentExecutionEngine,
    ) {
        let (sender, receiver) = mpsc::channel(1000);

        let db = Arc::new(DummyDB::new());
        let executor_factory = if !should_fail {
            MockIntentExecutorFactory::new()
        } else {
            MockIntentExecutorFactory::new_failing()
        };
        let worker = IntentExecutionEngine::new(
            db.clone(),
            executor_factory,
            None::<IntentPersisterImpl>,
            receiver,
        );

        (sender, worker)
    }

    #[tokio::test]
    async fn test_worker_processes_messages() {
        let (sender, worker) = setup_engine(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a test message
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        sender.send(msg.clone()).await.unwrap();

        // Verify the message was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.id, 1);
    }

    #[tokio::test]
    async fn test_worker_handles_conflicting_messages() {
        let (sender, worker) = setup_engine(false);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send two conflicting messages
        let pubkey = pubkey!("1111111111111111111111111111111111111111111");
        let msg1 = create_test_intent(1, &[pubkey]);
        let msg2 = create_test_intent(2, &[pubkey]);

        sender.send(msg1.clone()).await.unwrap();
        sender.send(msg2.clone()).await.unwrap();

        // First message should be processed immediately
        let result1 = result_receiver.recv().await.unwrap();
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap().id, 1);

        // Second message should be processed after first completes
        let result2 = result_receiver.recv().await.unwrap();
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap().id, 2);
    }

    #[tokio::test]
    async fn test_worker_handles_executor_failure() {
        let (sender, worker) = setup_engine(true);
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a test message that will fail
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        sender.send(msg.clone()).await.unwrap();

        // Verify the failure was properly reported
        let result = result_receiver.recv().await.unwrap();
        let Err((id, trigger_type, err)) = result else {
            panic!();
        };
        assert_eq!(id, 1);
        assert_eq!(trigger_type, TriggerType::OffChain);
        assert_eq!(
            err.to_string(),
            "FailedToCommitError: SignerError: custom error: oops"
        );
    }

    #[tokio::test]
    async fn test_worker_falls_back_to_db_when_channel_empty() {
        let (_sender, worker) = setup_engine(false);

        // Add a message to the DB
        let msg = create_test_intent(
            1,
            &[pubkey!("1111111111111111111111111111111111111111111")],
        );
        worker.db.store_base_intent(msg.clone()).await.unwrap();

        // Start worker
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Verify the message from DB was processed
        let result = result_receiver.recv().await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, 1);
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

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send a flood of messages
        for i in 0..NUM_MESSAGES {
            let msg = create_test_intent(
                i as u64,
                &[pubkey!("1111111111111111111111111111111111111111111")],
            );
            sender.send(msg).await.unwrap();
        }

        // Process results and verify constraints
        let mut completed = 0;
        while completed < NUM_MESSAGES {
            let result = result_receiver.recv().await.unwrap();
            assert!(result.is_ok());
            // Tasks are blocking so will complete sequentially
            assert_eq!(result.unwrap().id, completed as u64);
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
        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send several messages that will fail
        const NUM_FAILURES: usize = 10;
        for i in 0..NUM_FAILURES {
            let msg = create_test_intent(
                i as u64,
                &[pubkey!("1111111111111111111111111111111111111111111")],
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

        let result_subscriber = worker.spawn();
        let mut result_receiver = result_subscriber.subscribe();

        // Send messages with unique keys (non-blocking)
        let mut received_ids = HashSet::new();
        for i in 0..NUM_MESSAGES {
            let unique_pubkey = Pubkey::new_unique(); // Each message gets unique key
            let msg = create_test_intent(i, &[unique_pubkey]);

            received_ids.insert(i);
            sender.send(msg).await.unwrap();
        }

        // Process results
        let mut completed = 0;
        while completed < NUM_MESSAGES {
            let result = result_receiver.recv().await.unwrap();
            assert!(result.is_ok());

            // Message has to be present in set
            let id = result.unwrap().id;
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
        println!("max_observed: {}", max_observed);
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

            let msg = create_test_intent(i as u64, &pubkeys);
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

    // Mock implementations for testing
    pub struct MockIntentExecutorFactory {
        should_fail: bool,
        active_tasks: Option<Arc<AtomicUsize>>,
        max_concurrent: Option<Arc<AtomicUsize>>,
    }

    impl MockIntentExecutorFactory {
        pub fn new() -> Self {
            Self {
                should_fail: false,
                active_tasks: None,
                max_concurrent: None,
            }
        }

        pub fn new_failing() -> Self {
            Self {
                should_fail: true,
                active_tasks: None,
                max_concurrent: None,
            }
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
                active_tasks: self.active_tasks.clone(),
                max_concurrent: self.max_concurrent.clone(),
            }
        }
    }

    pub struct MockIntentExecutor {
        should_fail: bool,
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
    impl IntentExecutor for MockIntentExecutor {
        async fn execute<P: IntentPersister>(
            &self,
            _base_intent: ScheduledBaseIntent,
            _persister: Option<P>,
        ) -> IntentExecutorResult<ExecutionOutput> {
            self.on_task_started();

            // Simulate some work
            sleep(Duration::from_millis(50)).await;

            let result = if self.should_fail {
                Err(ExecutorError::FailedToCommitError {
                    err: InternalError::SignerError(SignerError::Custom(
                        "oops".to_string(),
                    )),
                    signature: None,
                })
            } else {
                Ok(ExecutionOutput::TwoStage {
                    commit_signature: Signature::default(),
                    finalize_signature: Signature::default(),
                })
            };

            self.on_task_finished();

            result
        }
    }

    #[derive(Clone)]
    pub struct MockInfoFetcher;
    #[async_trait]
    impl TaskInfoFetcher for MockInfoFetcher {
        async fn fetch_next_commit_ids(
            &self,
            pubkeys: &[Pubkey],
            _compressed: bool,
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys.iter().map(|pubkey| (*pubkey, 1)).collect())
        }

        async fn fetch_rent_reimbursements(
            &self,
            pubkeys: &[Pubkey],
        ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
            Ok(pubkeys.iter().map(|_| Pubkey::new_unique()).collect())
        }

        fn peek_commit_id(&self, _pubkey: &Pubkey) -> Option<u64> {
            None
        }

        fn reset(&self, _: ResetType) {}
    }
}
