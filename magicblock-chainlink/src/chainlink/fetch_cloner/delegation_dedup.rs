use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use magicblock_config::config::DelegationDedupConfig;
use parking_lot::Mutex;
use solana_signature::Signature;
use tokio::{sync::watch, time::Instant};
use tokio_util::{
    sync::CancellationToken,
    task::{task_tracker::TaskTrackerToken, TaskTracker},
};

use crate::{
    chainlink::errors::{ChainlinkError, ChainlinkResult},
    cloner::DelegationIdentity,
};

const CLEANUP_BATCH: usize = 256;
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

pub(super) type DelegationCloneResult = Result<Signature, Arc<ChainlinkError>>;
pub(super) type CompletionReceiver =
    watch::Receiver<Option<DelegationCloneResult>>;

enum Entry {
    Running(watch::Sender<Option<DelegationCloneResult>>),
    Finished {
        expires_at: Instant,
        result: DelegationCloneResult,
    },
}

#[derive(Default)]
struct State {
    // Running clones and retained results for each delegation.
    entries: HashMap<DelegationIdentity, Entry>,
    // Cleanup deadlines in expiry order; the results themselves live in `entries`.
    expiry_queue: VecDeque<(Instant, DelegationIdentity)>,
    running_count: usize,
    closed: bool,
}

/// Whether to start a delegation clone, wait for it, or reuse its result.
pub(super) enum DedupDecision {
    /// Own a new delegation clone. Move `guard` into a task whose lifetime is
    /// independent of this caller, and finish it with the clone result.
    /// Dropping it unfinished records abandonment. Use `completion` to wait
    /// for the same result.
    Start {
        guard: DelegationCloneGuard,
        completion: CompletionReceiver,
    },
    /// Another caller owns this delegation clone. Receive that owner's result
    /// without starting another clone or rescue attempt.
    Wait(CompletionReceiver),
    /// Return a completed clone's retained success or failure without new work.
    /// A retained success does not mean the target still exists or is usable;
    /// the caller must check its current state in the bank.
    Reuse(DelegationCloneResult),
}

/// Prevents duplicate delegation clones across the validator's clone paths.
/// A delegation clone includes dependency checks, cloning into the local bank
/// with post-delegation actions, and a rescue-undelegation attempt if those
/// actions cannot execute safely.
///
/// Post-delegation actions can immediately schedule commit and undelegation,
/// after which the account may be closed and removed locally. At that point,
/// neither the bank nor `pending_clones` remembers the completed clone:
/// the account is gone and the pending-clone entry was removed on completion.
/// A delayed request that already resolved the old delegation could otherwise
/// clone it again and repeat its actions. This type retains that history
/// independently of both the account and the pending-clone entry.
///
/// One instance is shared by all copies of `FetchCloner`, covering on-demand
/// fetches, account/program subscriptions, discovery, and ATA projection. Its
/// key is [`DelegationIdentity`]: the original delegated account and the
/// delegation record's slot, never a fetch or notification slot. Projected
/// ATAs use their underlying eATA identity, so discovery through either address
/// reaches the same record. A new delegation slot is a different identity.
///
/// [`Self::claim`] atomically assigns one owner. `FetchCloner` runs that owner's
/// dependency checks, clone, and possible rescue in a task that survives caller
/// cancellation. Duplicates join the running result or read the retained result;
/// they must not start another clone or rescue. The owner guard tracks the
/// task for shutdown draining and records failure if the owner disappears.
///
/// Running entries never expire. Completed successes and failures are retained
/// for the configured duration starting at completion; duplicate hits do not
/// extend it. A successful local-state skip that submitted no transaction is
/// not retained: a different authoritative delegation must not mark this one
/// as processed. Capacity limits reject new work rather than evicting running
/// or unexpired entries.
///
/// This is bounded, process-local protection: expiry or restart loses the
/// history, so it does not provide unconditional exactly-once execution. It
/// supplements the existing authority, freshness, and clone-serialization
/// checks. A retained success also does not imply that the account still exists
/// or is usable; callers must check the bank before reporting availability.
pub(crate) struct DelegationDeduplicator {
    config: DelegationDedupConfig,
    state: Mutex<State>,
    tasks: TaskTracker,
    stop_cleanup: CancellationToken,
}

impl DelegationDeduplicator {
    pub(crate) fn new(
        config: DelegationDedupConfig,
    ) -> ChainlinkResult<Arc<Self>> {
        if config.retention.is_zero()
            || Instant::now().checked_add(config.retention).is_none()
        {
            return Err(ChainlinkError::InvalidDelegationDedupConfig(
                "retention must be positive and fit the monotonic clock",
            ));
        }
        if config.max_entries == 0 || config.max_active == 0 {
            return Err(ChainlinkError::InvalidDelegationDedupConfig(
                "max-entries and max-active must be positive",
            ));
        }
        let deduplicator = Arc::new(Self {
            config,
            state: Mutex::new(State::default()),
            tasks: TaskTracker::new(),
            stop_cleanup: CancellationToken::new(),
        });
        let weak = Arc::downgrade(&deduplicator);
        let stop = deduplicator.stop_cleanup.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    _ = tokio::time::sleep(CLEANUP_INTERVAL) => {
                        let Some(deduplicator) = weak.upgrade() else { break };
                        deduplicator.state.lock().remove_expired(Instant::now());
                    }
                }
            }
        });
        Ok(deduplicator)
    }

    /// Atomically reserves a new delegation clone, or returns a decision to wait
    /// for or reuse an existing result. Its tracker token is acquired under the
    /// same lock as shutdown, so draining cannot miss a reserved owner task.
    pub(super) fn claim(
        self: &Arc<Self>,
        identity: DelegationIdentity,
    ) -> ChainlinkResult<DedupDecision> {
        let mut state = self.state.lock();
        let now = Instant::now();
        state.remove_expired(now);
        if matches!(state.entries.get(&identity), Some(Entry::Finished { expires_at, .. }) if *expires_at <= now)
        {
            state.entries.remove(&identity);
        }
        if let Some(entry) = state.entries.get(&identity) {
            return Ok(match entry {
                Entry::Running(sender) => {
                    DedupDecision::Wait(sender.subscribe())
                }
                Entry::Finished { result, .. } => {
                    DedupDecision::Reuse(result.clone())
                }
            });
        }
        if state.closed {
            return Err(ChainlinkError::DelegationDedupClosed);
        }
        let exhausted = if state.entries.len() >= self.config.max_entries {
            Some("max-entries")
        } else if state.running_count >= self.config.max_active {
            Some("max-active")
        } else {
            None
        };
        if let Some(limit) = exhausted {
            return Err(ChainlinkError::DelegationDedupCapacityExceeded(limit));
        }
        let (sender, completion) = watch::channel(None);
        state.entries.insert(identity, Entry::Running(sender));
        state.running_count += 1;
        Ok(DedupDecision::Start {
            guard: DelegationCloneGuard {
                deduplicator: self.clone(),
                identity,
                finished: false,
                _task: self.tasks.token(),
            },
            completion,
        })
    }

    fn finish(
        &self,
        identity: DelegationIdentity,
        result: DelegationCloneResult,
    ) {
        let sender = {
            let mut state = self.state.lock();
            let Some(Entry::Running(sender)) = state.entries.remove(&identity)
            else {
                return;
            };
            state.running_count -= 1;
            // A default signature is the lower clone layer's local-state
            // short circuit: no transaction ran. In particular, a different
            // authoritative generation must not mark this one as processed.
            let retain = !matches!(
                &result,
                Ok(signature) if *signature == Signature::default()
            );
            if retain {
                let expires_at = Instant::now() + self.config.retention;
                state.entries.insert(
                    identity,
                    Entry::Finished {
                        expires_at,
                        result: result.clone(),
                    },
                );
                state.expiry_queue.push_back((expires_at, identity));
            }
            sender
        };
        // Publish before dropping the tracker token so shutdown includes result delivery.
        sender.send_replace(Some(result));
    }

    pub(super) fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        self.tasks.close();
        self.stop_cleanup.cancel();
    }

    pub(super) async fn drain(&self) {
        self.tasks.wait().await;
    }
}

impl State {
    fn remove_expired(&mut self, now: Instant) {
        for _ in 0..CLEANUP_BATCH {
            let Some(&(expires_at, identity)) = self.expiry_queue.front()
            else {
                break;
            };
            if expires_at > now {
                break;
            }
            self.expiry_queue.pop_front();
            // An expired key may already have been reclaimed by a lookup and
            // claimed again. An old queue item must not remove its new entry.
            if matches!(self.entries.get(&identity), Some(Entry::Finished { expires_at: current, .. }) if *current == expires_at)
            {
                self.entries.remove(&identity);
            }
        }
    }
}

impl Drop for DelegationDeduplicator {
    fn drop(&mut self) {
        self.stop_cleanup.cancel();
    }
}

pub(super) struct DelegationCloneGuard {
    deduplicator: Arc<DelegationDeduplicator>,
    identity: DelegationIdentity,
    finished: bool,
    _task: TaskTrackerToken,
}

impl DelegationCloneGuard {
    pub(super) fn finish(mut self, result: DelegationCloneResult) {
        self.deduplicator.finish(self.identity, result);
        self.finished = true;
    }
}

impl Drop for DelegationCloneGuard {
    fn drop(&mut self) {
        if !self.finished {
            tracing::error!(
                delegated_account = %self.identity.delegated_account,
                delegation_slot = self.identity.delegation_slot,
                "Delegation clone owner terminated without a result"
            );
            self.deduplicator.finish(
                self.identity,
                Err(Arc::new(ChainlinkError::DelegationCloneAbandoned(
                    self.identity,
                ))),
            );
        }
    }
}

pub(super) async fn wait_for_completion(
    mut completion: CompletionReceiver,
    identity: DelegationIdentity,
) -> DelegationCloneResult {
    loop {
        if let Some(result) = completion.borrow_and_update().clone() {
            return result;
        }
        if completion.changed().await.is_err() {
            return Err(Arc::new(ChainlinkError::DelegationCloneAbandoned(
                identity,
            )));
        }
    }
}
