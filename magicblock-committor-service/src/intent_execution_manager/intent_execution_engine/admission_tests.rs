use std::{
    collections::HashSet,
    future::{poll_fn, Future},
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

use async_trait::async_trait;
use futures_util::poll;
use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_pubkey::Pubkey;
use solana_transaction_error::TransactionError;
use tokio::sync::oneshot;

use super::*;
use crate::{
    intent_execution_manager::{
        db::DummyDB, intent_scheduler::create_test_intent,
    },
    intent_executor::error::InternalError,
    persist::IntentPersisterImpl,
    transaction_preparator::delivery_preparator::BufferExecutionError,
};

type TestEngine = IntentExecutionEngine<
    DummyDB,
    IntentPersisterImpl,
    ControlledExecutorFactory,
>;

#[derive(Debug)]
struct Attempt {
    id: u64,
    finish: oneshot::Sender<bool>,
}

async fn next_attempt(
    attempts: &mut mpsc::UnboundedReceiver<Attempt>,
) -> Attempt {
    tokio::time::timeout(Duration::from_secs(5), attempts.recv())
        .await
        .expect("expected an execution to start")
        .expect("execution event channel closed")
}

impl Attempt {
    fn succeed(self) {
        self.finish.send(true).unwrap();
    }

    fn fail_transiently(self) {
        self.finish.send(false).unwrap();
    }
}

struct ControlledExecutorFactory(mpsc::UnboundedSender<Attempt>);

impl IntentExecutorFactory for ControlledExecutorFactory {
    type Executor = ControlledExecutor;

    fn create_instance(&self) -> Self::Executor {
        ControlledExecutor(self.0.clone())
    }
}

struct ControlledExecutor(mpsc::UnboundedSender<Attempt>);

#[async_trait]
impl IntentExecutor for ControlledExecutor {
    async fn execute<P: IntentPersister>(
        &mut self,
        intent: ScheduledIntentBundle,
        _persister: Option<P>,
    ) -> IntentExecutionResult {
        let (finish, finished) = oneshot::channel();
        self.0
            .send(Attempt {
                id: intent.id,
                finish,
            })
            .unwrap();
        let success = finished.await.unwrap();
        IntentExecutionResult {
            inner: if success {
                Ok(ExecutionOutput::TwoStage {
                    commit_signature: Signature::default(),
                    finalize_signature: Signature::default(),
                })
            } else {
                Err(IntentExecutorError::FailedToCommitError {
                    err: TransactionStrategyExecutionError::InternalError(
                        InternalError::from(
                            MagicBlockRpcClientError::SentTransactionError(
                                TransactionError::BlockhashNotFound,
                                Signature::default(),
                            ),
                        ),
                    ),
                    signature: None,
                })
            },
            patched_errors: vec![],
            callbacks_report: vec![],
        }
    }

    async fn cleanup(self) -> Result<(), BufferExecutionError> {
        Ok(())
    }
}

fn setup() -> (
    mpsc::Sender<ScheduledIntentBundle>,
    TestEngine,
    mpsc::UnboundedReceiver<Attempt>,
) {
    let (sender, receiver) = mpsc::channel(1000);
    let (started, attempts) = mpsc::unbounded_channel();
    let engine = IntentExecutionEngine::new(
        Arc::new(DummyDB::new()),
        ControlledExecutorFactory(started),
        None::<IntentPersisterImpl>,
        receiver,
    );
    (sender, engine, attempts)
}

struct RunningEngine {
    handle: JoinHandle<()>,
    polls: Arc<AtomicUsize>,
    results: broadcast::Receiver<BroadcastedIntentExecutionResult>,
}

impl RunningEngine {
    async fn next_result(&mut self) -> BroadcastedIntentExecutionResult {
        tokio::time::timeout(Duration::from_secs(5), self.results.recv())
            .await
            .expect("expected an execution result")
            .expect("execution result channel closed or lagged")
    }
}

impl Drop for RunningEngine {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn run(engine: TestEngine) -> RunningEngine {
    let (sender, results) = broadcast::channel(100);
    let polls = Arc::new(AtomicUsize::new(0));
    let poll_count = polls.clone();
    let mut main_loop = Box::pin(engine.main_loop(sender));
    let handle = tokio::spawn(poll_fn(move |cx| {
        poll_count.fetch_add(1, Ordering::SeqCst);
        main_loop.as_mut().poll(cx)
    }));
    RunningEngine {
        handle,
        polls,
        results,
    }
}

async fn wait_for_blocked(inner: &Mutex<IntentScheduler>, count: usize) {
    for _ in 0..10_000 {
        if inner.lock().unwrap().intents_blocked() == count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("scheduler did not reach {count} blocked intents");
}

async fn assert_parked(running: &RunningEngine) {
    tokio::task::yield_now().await;
    let polls = running.polls.load(Ordering::SeqCst);
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
    assert_eq!(running.polls.load(Ordering::SeqCst), polls);
}

fn fill_blocked(engine: &TestEngine, count: usize) -> Pubkey {
    let key = Pubkey::new_unique();
    let mut scheduler = engine.inner.lock().unwrap();
    assert!(scheduler
        .schedule(create_test_intent(0, &[key], false))
        .is_some());
    for id in 1..=count as u64 {
        assert!(scheduler
            .schedule(create_test_intent(id, &[key], false))
            .is_none());
    }
    key
}

#[tokio::test]
async fn admission_respects_normal_and_hard_boundaries() {
    for (blocked, free, admitted) in [
        (SCHEDULER_CAPACITY - 1, 0, true),
        (SCHEDULER_CAPACITY - 1, 1, true),
        (SCHEDULER_CAPACITY, 0, false),
        (SCHEDULER_CAPACITY, 1, true),
        (SCHEDULER_HARD_CAPACITY - 1, 0, false),
        (SCHEDULER_HARD_CAPACITY - 1, 1, true),
        (SCHEDULER_HARD_CAPACITY, 0, false),
        (SCHEDULER_HARD_CAPACITY, 1, false),
    ] {
        let (sender, mut engine, _attempts) = setup();
        let key = fill_blocked(&engine, blocked);
        let _permits = engine
            .executors_semaphore
            .clone()
            .acquire_many_owned(MAX_EXECUTORS as u32 - free)
            .await
            .unwrap();
        sender
            .send(create_test_intent(100_000, &[key], false))
            .await
            .unwrap();
        {
            let mut next = Box::pin(engine.next_scheduled_intent());
            if admitted {
                assert!(matches!(poll!(next.as_mut()), Poll::Ready(Ok(None))));
            } else {
                assert!(poll!(next.as_mut()).is_pending());
            }
        }
        assert_eq!(
            engine.inner.lock().unwrap().intents_blocked(),
            blocked + usize::from(admitted)
        );
        assert_eq!(engine.receiver.len(), usize::from(!admitted));
    }
}

#[tokio::test]
async fn capacity_notification_wakes_before_or_after_waiting() {
    for notify_before_poll in [true, false] {
        let (sender, mut engine, _attempts) = setup();
        fill_blocked(&engine, SCHEDULER_CAPACITY);
        let permits = engine
            .executors_semaphore
            .clone()
            .acquire_many_owned(MAX_EXECUTORS as u32)
            .await
            .unwrap();
        let notification = engine.execution_capacity_changed.clone();
        sender
            .send(create_test_intent(100_000, &[Pubkey::new_unique()], false))
            .await
            .unwrap();

        let mut next = Box::pin(engine.next_scheduled_intent());
        if !notify_before_poll {
            assert!(poll!(next.as_mut()).is_pending());
        }
        drop(permits);
        notification.notify_one();
        assert!(matches!(poll!(next.as_mut()), Poll::Ready(Ok(None))));
        drop(next);
        assert_eq!(
            engine.next_scheduled_intent().await.unwrap().unwrap().id,
            100_000
        );
    }
}

#[tokio::test]
async fn independent_intent_runs_beyond_normal_capacity() {
    let (_sender, engine, mut attempts) = setup();
    let hot = Pubkey::new_unique();
    let independent_id = SCHEDULER_CAPACITY as u64 + 2;
    let mut pending: Vec<_> = (0..independent_id)
        .map(|id| create_test_intent(id, &[hot], false))
        .collect();
    pending.push(create_test_intent(
        independent_id,
        &[Pubkey::new_unique()],
        false,
    ));
    engine.db.store_intent_bundles(pending).await.unwrap();
    let inner = engine.inner.clone();
    let mut running = run(engine);

    let blocker = next_attempt(&mut attempts).await;
    assert_eq!(blocker.id, 0);
    let independent = next_attempt(&mut attempts).await;
    assert_eq!(independent.id, independent_id);
    assert_eq!(
        inner.lock().unwrap().intents_blocked(),
        SCHEDULER_CAPACITY + 1
    );
    independent.succeed();
    assert_eq!(running.next_result().await.id, independent_id);
    // The original execution is still held, so none of its successors can run.
    assert!(attempts.try_recv().is_err());
    running.handle.abort();
    blocker.succeed();
}

#[tokio::test]
async fn conflicting_backlog_stops_at_hard_capacity_without_polling() {
    let (_sender, engine, mut attempts) = setup();
    let hot = Pubkey::new_unique();
    let pending = (0..=SCHEDULER_HARD_CAPACITY as u64 + 1)
        .map(|id| create_test_intent(id, &[hot], false))
        .collect();
    engine.db.store_intent_bundles(pending).await.unwrap();
    let inner = engine.inner.clone();
    let db = engine.db.clone();
    let running = run(engine);
    let blocker = next_attempt(&mut attempts).await;

    wait_for_blocked(&inner, SCHEDULER_HARD_CAPACITY).await;
    assert_parked(&running).await;
    assert!(attempts.try_recv().is_err());
    assert_eq!(
        db.pop_intent_bundle().await.unwrap().unwrap().id,
        SCHEDULER_HARD_CAPACITY as u64 + 1
    );
    assert!(
        running.polls.load(Ordering::SeqCst)
            >= SCHEDULER_HARD_CAPACITY / SCHEDULING_BATCH_SIZE
    );
    running.handle.abort();
    blocker.succeed();
}

#[tokio::test]
async fn one_completion_dispatches_all_ready_successors_once() {
    let (_sender, engine, mut attempts) = setup();
    let keys = [
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    ];
    let pending = vec![
        create_test_intent(0, &keys, false),
        create_test_intent(1, &keys[0..1], false),
        create_test_intent(2, &keys[1..2], false),
        create_test_intent(2, &keys[1..2], false),
        create_test_intent(3, &keys[2..3], false),
    ];
    engine.db.store_intent_bundles(pending).await.unwrap();
    let inner = engine.inner.clone();
    let mut running = run(engine);
    let blocker = next_attempt(&mut attempts).await;
    wait_for_blocked(&inner, 3).await;
    blocker.succeed();

    let mut successors = Vec::new();
    for _ in 0..3 {
        successors.push(next_attempt(&mut attempts).await);
    }
    assert_eq!(
        successors
            .iter()
            .map(|attempt| attempt.id)
            .collect::<HashSet<_>>(),
        HashSet::from([1, 2, 3])
    );
    for attempt in successors {
        attempt.succeed();
    }
    let mut results = HashSet::new();
    for _ in 0..4 {
        let result = running.next_result().await;
        assert!(result.is_ok());
        assert!(results.insert(result.id));
    }
    assert_parked(&running).await;
    assert!(attempts.try_recv().is_err());
}

#[tokio::test]
async fn transitive_order_is_preserved_across_channel_and_backlog() {
    let (sender, engine, mut attempts) = setup();
    let [a, b, c] = [
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    ];
    sender
        .send(create_test_intent(0, &[a], false))
        .await
        .unwrap();
    sender
        .send(create_test_intent(1, &[a, b], false))
        .await
        .unwrap();
    engine
        .db
        .store_intent_bundles(vec![
            create_test_intent(2, &[b], false),
            create_test_intent(3, &[c], false),
        ])
        .await
        .unwrap();
    let inner = engine.inner.clone();
    let mut running = run(engine);

    let first = next_attempt(&mut attempts).await;
    assert_eq!(first.id, 0);
    let independent = next_attempt(&mut attempts).await;
    assert_eq!(independent.id, 3);
    wait_for_blocked(&inner, 2).await;
    independent.succeed();
    assert_eq!(running.next_result().await.id, 3);
    assert_parked(&running).await;
    assert!(attempts.try_recv().is_err());
    first.succeed();
    let second = next_attempt(&mut attempts).await;
    assert_eq!(second.id, 1);
    assert_parked(&running).await;
    assert!(attempts.try_recv().is_err());
    second.succeed();
    let third = next_attempt(&mut attempts).await;
    assert_eq!(third.id, 2);
    third.succeed();
    for id in [0, 1, 2] {
        assert_eq!(running.next_result().await.id, id);
    }
}

#[tokio::test(start_paused = true)]
async fn retry_release_resumes_admission_with_all_tasks_still_running() {
    let (_sender, engine, mut attempts) = setup();
    let hot = Pubkey::new_unique();
    let mut pending = vec![create_test_intent(0, &[hot], false)];
    for id in 1..MAX_EXECUTORS as u64 {
        pending.push(create_test_intent(id, &[Pubkey::new_unique()], false));
    }
    let independent_id = MAX_EXECUTORS as u64 + SCHEDULER_CAPACITY as u64;
    for id in MAX_EXECUTORS as u64..independent_id {
        pending.push(create_test_intent(id, &[hot], false));
    }
    // The second independent intent must wait: one released permit permits
    // only one replacement execution, despite the retry still being tracked.
    for id in independent_id..=independent_id + 1 {
        pending.push(create_test_intent(id, &[Pubkey::new_unique()], false));
    }
    engine.db.store_intent_bundles(pending).await.unwrap();
    let inner = engine.inner.clone();
    let mut running = run(engine);
    let mut active = Vec::new();
    let blocker = next_attempt(&mut attempts).await;
    assert_eq!(blocker.id, 0);
    for _ in 1..MAX_EXECUTORS {
        active.push(next_attempt(&mut attempts).await);
    }
    wait_for_blocked(&inner, SCHEDULER_CAPACITY).await;
    assert_parked(&running).await;
    let now = tokio::time::Instant::now();
    blocker.fail_transiently();

    let independent = next_attempt(&mut attempts).await;
    assert_eq!(independent.id, independent_id);
    assert_eq!(tokio::time::Instant::now(), now);
    assert_eq!(inner.lock().unwrap().intents_blocked(), SCHEDULER_CAPACITY);
    assert_parked(&running).await;
    assert!(attempts.try_recv().is_err());
    assert!(running.results.try_recv().is_err());
    // The original retry and all conflicting successors remain pending.
    running.handle.abort();
    independent.succeed();
    for attempt in active {
        attempt.succeed();
    }
}
