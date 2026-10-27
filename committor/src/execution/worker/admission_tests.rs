use std::{
    collections::HashSet,
    future::{Future, poll_fn},
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

use async_trait::async_trait;
use futures_util::poll;
use rpc_client::MagicBlockRpcClientError;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction_error::TransactionError;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use super::*;
use crate::execution::{
    backlog::{BacklogDB, DummyDB},
    error::InternalError,
    progress::TransactionProgress,
    scheduler::create_test_intent_bundle,
};

#[derive(Debug)]
struct Attempt {
    id: u64,
    finish: oneshot::Sender<bool>,
}

async fn next_attempt(attempts: &mut mpsc::UnboundedReceiver<Attempt>) -> Attempt {
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

#[derive(Clone)]
struct ControlledExecutor(mpsc::UnboundedSender<Attempt>);

#[async_trait]
impl IntentRunner for ControlledExecutor {
    async fn execute(self: Arc<Self>, intent: &OutboxIntentBundle) -> IntentExecutionResult {
        let (finish, finished) = oneshot::channel();
        self.0.send(Attempt { id: intent.intent_id, finish }).unwrap();
        let success = finished.await.unwrap();
        let output = ExecutionOutput {
            confirmed_commit: Some(Signature::default()),
            signature: Signature::default(),
        };
        IntentExecutionResult {
            inner: if success {
                Ok(output)
            } else {
                Err(IntentExecutorError::FailedExecution(
                    TransactionExecutionError::InternalError(InternalError::from(
                        MagicBlockRpcClientError::SentTransactionError(
                            TransactionError::BlockhashNotFound,
                            Signature::default(),
                        ),
                    )),
                ))
            },
            progress: if success {
                output.progress()
            } else {
                ExecutionProgress {
                    transaction: TransactionProgress::Rejected(Signature::default()),
                    ..Default::default()
                }
            },
            callbacks_reported: false,
        }
    }
}

fn setup() -> (
    mpsc::Sender<OutboxIntentBundle>,
    IntentExecutor,
    mpsc::UnboundedReceiver<Attempt>,
    Arc<Mutex<DummyDB>>,
) {
    let (sender, receiver) = mpsc::channel(1000);
    let (started, attempts) = mpsc::unbounded_channel();
    let db = Arc::new(Mutex::new(DummyDB::new()));
    let engine = IntentExecutor::new(
        IntentStream::new(db.clone(), receiver),
        Arc::new(ControlledExecutor(started)),
        tokio_util::task::TaskTracker::new(),
    );
    (sender, engine, attempts, db)
}

struct RunningEngine {
    handle: JoinHandle<()>,
    polls: Arc<AtomicUsize>,
    results: broadcast::Receiver<RecordedResult>,
}

impl RunningEngine {
    async fn next_result(&mut self) -> RecordedResult {
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

fn run(engine: IntentExecutor) -> RunningEngine {
    let results = engine.result_sender.subscribe();
    let polls = Arc::new(AtomicUsize::new(0));
    let poll_count = polls.clone();
    let handle = tokio::spawn(async move {
        let mut engine = engine;
        let mut main_loop = Box::pin(engine.main_loop());
        poll_fn(move |cx| {
            poll_count.fetch_add(1, Ordering::SeqCst);
            main_loop.as_mut().poll(cx)
        })
        .await
        .unwrap();
    });
    RunningEngine { handle, polls, results }
}

async fn wait_for_blocked(inner: &Mutex<IntentScheduler>, count: usize) {
    for _ in 0..10_000 {
        if inner.lock().intents_blocked() == count {
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

fn fill_blocked(engine: &IntentExecutor, count: usize) -> Pubkey {
    let key = Pubkey::new_unique();
    let mut scheduler = engine.scheduler.lock();
    assert!(scheduler.schedule(create_test_intent_bundle(0, &[key], &[])).is_some());
    for id in 1..=count as u64 {
        assert!(scheduler.schedule(create_test_intent_bundle(id, &[key], &[])).is_none());
    }
    key
}

/// Proves normal and hard admission boundaries with and without execution capacity.
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
        let (sender, mut engine, _attempts, _db) = setup();
        let key = fill_blocked(&engine, blocked);
        let _permits = engine
            .executors_semaphore
            .clone()
            .acquire_many_owned(MAX_EXECUTORS as u32 - free)
            .await
            .unwrap();
        sender.send(create_test_intent_bundle(100_000, &[key], &[])).await.unwrap();
        {
            let mut next = Box::pin(engine.next_scheduled_intent());
            if admitted {
                assert!(matches!(poll!(next.as_mut()), Poll::Ready(Ok(None))));
            } else {
                assert!(poll!(next.as_mut()).is_pending());
            }
        }
        assert_eq!(
            engine.scheduler.lock().intents_blocked(),
            blocked + usize::from(admitted)
        );
        assert_eq!(
            sender.capacity(),
            sender.max_capacity() - usize::from(!admitted)
        );
    }
}

/// Proves independent work can use spare executors beyond normal capacity.
#[tokio::test]
async fn independent_intent_runs_beyond_normal_capacity() {
    let (_sender, engine, mut attempts, db) = setup();
    let hot = Pubkey::new_unique();
    let independent_id = SCHEDULER_CAPACITY as u64 + 2;
    let mut pending: Vec<_> = (0..independent_id)
        .map(|id| create_test_intent_bundle(id, &[hot], &[]))
        .collect();
    pending.push(create_test_intent_bundle(
        independent_id,
        &[Pubkey::new_unique()],
        &[],
    ));
    db.lock().store_intent_bundles(pending).unwrap();
    let inner = engine.scheduler.clone();
    let mut running = run(engine);

    let blocker = next_attempt(&mut attempts).await;
    assert_eq!(blocker.id, 0);
    let independent = next_attempt(&mut attempts).await;
    assert_eq!(independent.id, independent_id);
    assert_eq!(inner.lock().intents_blocked(), SCHEDULER_CAPACITY + 1);
    independent.succeed();
    assert_eq!(running.next_result().await.id, independent_id);
    // The original execution is still held, so none of its successors can run.
    assert!(attempts.try_recv().is_err());
    running.handle.abort();
    blocker.succeed();
}

/// Proves a conflicting backlog parks at the hard resource bound.
#[tokio::test]
async fn conflicting_backlog_stops_at_hard_capacity_without_polling() {
    let (_sender, engine, mut attempts, db) = setup();
    let hot = Pubkey::new_unique();
    let pending = (0..=SCHEDULER_HARD_CAPACITY as u64 + 1)
        .map(|id| create_test_intent_bundle(id, &[hot], &[]))
        .collect();
    db.lock().store_intent_bundles(pending).unwrap();
    let inner = engine.scheduler.clone();
    let running = run(engine);
    let blocker = next_attempt(&mut attempts).await;

    wait_for_blocked(&inner, SCHEDULER_HARD_CAPACITY).await;
    assert_parked(&running).await;
    assert!(attempts.try_recv().is_err());
    assert_eq!(
        db.lock().pop_intent_bundle().unwrap().unwrap().intent_id,
        SCHEDULER_HARD_CAPACITY as u64 + 1
    );
    assert!(
        running.polls.load(Ordering::SeqCst) >= SCHEDULER_HARD_CAPACITY / SCHEDULING_BATCH_SIZE
    );
    running.handle.abort();
    blocker.succeed();
}

/// Proves one completion drains every eligible successor in dependency order.
#[tokio::test]
async fn one_completion_dispatches_successors_in_dependency_order() {
    let (sender, engine, mut attempts, db) = setup();
    let [a, b, c] = [Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()];
    for (id, keys) in [(0, [a, c]), (1, [a, b])] {
        sender.send(create_test_intent_bundle(id, &keys, &[])).await.unwrap();
    }
    let pending =
        vec![create_test_intent_bundle(2, &[c], &[]), create_test_intent_bundle(3, &[b], &[])];
    db.lock().store_intent_bundles(pending).unwrap();
    let inner = engine.scheduler.clone();
    let mut running = run(engine);
    let blocker = next_attempt(&mut attempts).await;
    assert_eq!(blocker.id, 0);
    wait_for_blocked(&inner, 3).await;
    blocker.succeed();

    // Both successors must start before either completes.
    let mut successors = [next_attempt(&mut attempts).await, next_attempt(&mut attempts).await];
    successors.sort_by_key(|attempt| attempt.id);
    let [first, second] = successors;
    assert_eq!([first.id, second.id], [1, 2]);
    second.succeed();
    assert_parked(&running).await;
    // Intent 3 shares B with intent 1, so it remains transitively blocked.
    assert!(attempts.try_recv().is_err());
    first.succeed();
    let third = next_attempt(&mut attempts).await;
    assert_eq!(third.id, 3);
    third.succeed();
    let mut results = HashSet::new();
    for _ in 0..4 {
        let result = running.next_result().await;
        assert!(result.is_ok());
        assert!(results.insert(result.id));
    }
    assert_parked(&running).await;
    assert!(attempts.try_recv().is_err());
}

/// Proves releasing a retry permit wakes admission without completing its task.
#[tokio::test(start_paused = true)]
async fn retry_release_resumes_admission_with_all_tasks_still_running() {
    let (_sender, engine, mut attempts, db) = setup();
    let hot = Pubkey::new_unique();
    let mut pending = vec![create_test_intent_bundle(0, &[hot], &[])];
    for id in 1..MAX_EXECUTORS as u64 {
        pending.push(create_test_intent_bundle(id, &[Pubkey::new_unique()], &[]));
    }
    let independent_id = MAX_EXECUTORS as u64 + SCHEDULER_CAPACITY as u64;
    for id in MAX_EXECUTORS as u64..independent_id {
        pending.push(create_test_intent_bundle(id, &[hot], &[]));
    }
    // The second independent intent must wait: one released permit permits
    // only one replacement execution, despite the retry still being tracked.
    for id in independent_id..=independent_id + 1 {
        pending.push(create_test_intent_bundle(id, &[Pubkey::new_unique()], &[]));
    }
    db.lock().store_intent_bundles(pending).unwrap();
    let inner = engine.scheduler.clone();
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
    assert_eq!(inner.lock().intents_blocked(), SCHEDULER_CAPACITY);
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
