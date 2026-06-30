use std::{collections::HashMap as StdHashMap, sync::Arc, time::Duration};

use magicblock_metrics::metrics::{
    self, AccountFetchOrigin, ChainlinkPendingFetchLayer,
    ChainlinkPendingFetchOutcome,
};
use scc::HashMap;
use solana_pubkey::Pubkey;
use tokio::{
    sync::{oneshot, Notify},
    time::Instant,
};

use super::{super::errors::ChainlinkError, types::FetchAndCloneResult};

pub(super) type PendingGeneration = u64;
pub(super) type WaiterId = u64;

pub(super) struct Pending {
    pub generation: PendingGeneration,
    pub deadline: Instant,
    pub waiters: StdHashMap<WaiterId, oneshot::Sender<PendingTerminal>>,
    pub cancel: Arc<Notify>,
}

#[derive(Debug, Clone)]
pub(super) enum PendingTerminal {
    Success(FetchAndCloneResult),
    Failed(PendingFailure),
}

#[derive(Debug, Clone)]
pub(super) enum PendingFailure {
    OwnerFailed(String),
    TimedOut,
    Cancelled,
}

impl PendingFailure {
    pub(super) fn into_chainlink_error(self, pubkey: Pubkey) -> ChainlinkError {
        match self {
            Self::OwnerFailed(msg) => {
                ChainlinkError::PendingRequestOwnerFailed(pubkey, msg)
            }
            Self::TimedOut => ChainlinkError::PendingRequestTimeout(pubkey),
            Self::Cancelled => ChainlinkError::PendingRequestCancelled(pubkey),
        }
    }
}

#[derive(Clone, Copy)]
struct PendingFetchMetricLabels {
    origin: AccountFetchOrigin,
    layer: ChainlinkPendingFetchLayer,
}

pub(super) struct PendingWaiter {
    pending: Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    waiter_id: WaiterId,
    receiver: oneshot::Receiver<PendingTerminal>,
    completed: bool,
    origin: AccountFetchOrigin,
    layer: ChainlinkPendingFetchLayer,
    active_waiter_gauge: bool,
}

impl PendingWaiter {
    fn new(
        pending: Arc<HashMap<Pubkey, Pending>>,
        pubkey: Pubkey,
        generation: PendingGeneration,
        waiter_id: WaiterId,
        receiver: oneshot::Receiver<PendingTerminal>,
        labels: PendingFetchMetricLabels,
        active_waiter_gauge: bool,
    ) -> Self {
        Self {
            pending,
            pubkey,
            generation,
            waiter_id,
            receiver,
            completed: false,
            origin: labels.origin,
            layer: labels.layer,
            active_waiter_gauge,
        }
    }

    pub(super) fn pubkey(&self) -> Pubkey {
        self.pubkey
    }

    pub(super) fn generation(&self) -> PendingGeneration {
        self.generation
    }

    fn finish_active_waiter(&mut self) {
        let _origin = self.origin;
        if self.active_waiter_gauge {
            metrics::dec_chainlink_pending_fetch_waiters_gauge(self.layer);
            self.active_waiter_gauge = false;
        }
    }

    pub(super) async fn wait(
        mut self,
    ) -> Result<PendingTerminal, ChainlinkError> {
        let receiver =
            std::mem::replace(&mut self.receiver, oneshot::channel().1);
        match receiver.await {
            Ok(terminal) => {
                self.finish_active_waiter();
                self.completed = true;
                Ok(terminal)
            }
            Err(err) => {
                self.finish_active_waiter();
                self.completed = true;
                Err(ChainlinkError::PendingRequestOwnerDisappeared(
                    self.pubkey,
                    err.to_string(),
                ))
            }
        }
    }
}

impl Drop for PendingWaiter {
    fn drop(&mut self) {
        if self.completed {
            return;
        }

        self.finish_active_waiter();
        self.pending.update(&self.pubkey, |_, pending| {
            if pending.generation == self.generation {
                pending.waiters.remove(&self.waiter_id);
            }
        });
    }
}

pub(super) struct PendingOwner {
    pending: Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    completed: bool,
    origin: AccountFetchOrigin,
    layer: ChainlinkPendingFetchLayer,
    started_at: std::time::Instant,
}

impl PendingOwner {
    fn new(
        pending: Arc<HashMap<Pubkey, Pending>>,
        pubkey: Pubkey,
        generation: PendingGeneration,
        origin: AccountFetchOrigin,
        layer: ChainlinkPendingFetchLayer,
    ) -> Self {
        Self {
            pending,
            pubkey,
            generation,
            completed: false,
            origin,
            layer,
            started_at: std::time::Instant::now(),
        }
    }

    pub(super) fn observe(&mut self, outcome: ChainlinkPendingFetchOutcome) {
        if self.completed {
            return;
        }
        metrics::observe_chainlink_pending_fetch_owner_duration_seconds(
            self.origin,
            self.layer,
            outcome,
            self.started_at.elapsed().as_secs_f64(),
        );
    }

    pub(super) fn finish(&mut self, outcome: ChainlinkPendingFetchOutcome) {
        self.observe(outcome);
        self.completed = true;
    }
}

impl Drop for PendingOwner {
    fn drop(&mut self) {
        if self.completed {
            return;
        }

        self.observe(ChainlinkPendingFetchOutcome::OwnerCancelled);
        let _ = finish_pending_cleanup(
            &self.pending,
            self.pubkey,
            self.generation,
            None,
        );
    }
}

pub(super) struct PendingHandles {
    pub waiter: PendingWaiter,
    pub deadline: Instant,
    pub cancel: Arc<Notify>,
    pub owner: Option<PendingOwner>,
}

pub(super) enum PendingClaim {
    Created(PendingHandles),
    Joined(PendingHandles),
}

pub(super) fn claim_or_join_pending(
    pending: Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    waiter_id: WaiterId,
    total_budget: Duration,
    origin: AccountFetchOrigin,
    layer: ChainlinkPendingFetchLayer,
) -> PendingClaim {
    let (tx, rx) = oneshot::channel();
    let labels = PendingFetchMetricLabels { origin, layer };

    let claim = match pending.entry(pubkey) {
        scc::hash_map::Entry::Vacant(entry) => {
            let deadline = Instant::now() + total_budget;
            let cancel = Arc::new(Notify::new());
            entry.insert_entry(Pending {
                generation,
                deadline,
                waiters: StdHashMap::from([(waiter_id, tx)]),
                cancel: Arc::clone(&cancel),
            });
            metrics::inc_chainlink_pending_fetch_accounts(
                origin,
                layer,
                ChainlinkPendingFetchOutcome::Owned,
                1,
            );
            PendingClaim::Created(PendingHandles {
                waiter: PendingWaiter::new(
                    pending.clone(),
                    pubkey,
                    generation,
                    waiter_id,
                    rx,
                    labels,
                    false,
                ),
                deadline,
                cancel,
                owner: Some(PendingOwner::new(
                    pending.clone(),
                    pubkey,
                    generation,
                    origin,
                    layer,
                )),
            })
        }
        scc::hash_map::Entry::Occupied(mut entry) => {
            let generation = entry.get().generation;
            let deadline = entry.get().deadline;
            let cancel = Arc::clone(&entry.get().cancel);
            entry.get_mut().waiters.insert(waiter_id, tx);
            metrics::inc_chainlink_pending_fetch_accounts(
                origin,
                layer,
                ChainlinkPendingFetchOutcome::JoinedExisting,
                1,
            );
            metrics::inc_chainlink_pending_fetch_waiters(origin, layer, 1);
            metrics::inc_chainlink_pending_fetch_waiters_gauge(layer);
            PendingClaim::Joined(PendingHandles {
                waiter: PendingWaiter::new(
                    pending.clone(),
                    pubkey,
                    generation,
                    waiter_id,
                    rx,
                    labels,
                    true,
                ),
                deadline,
                cancel,
                owner: None,
            })
        }
    };

    claim
}

fn finish_pending_cleanup(
    pending: &Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    terminal: Option<PendingTerminal>,
) -> usize {
    if let Some((_, pending)) =
        pending.remove_if(&pubkey, |pending| pending.generation == generation)
    {
        let waiter_count = pending.waiters.len();
        if let Some(terminal) = terminal {
            for (_, waiter) in pending.waiters {
                let _ = waiter.send(terminal.clone());
            }
        }
        waiter_count
    } else {
        0
    }
}

pub(super) fn finish_pending(
    pending: &Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    terminal: PendingTerminal,
) -> usize {
    finish_pending_cleanup(pending, pubkey, generation, Some(terminal))
}
