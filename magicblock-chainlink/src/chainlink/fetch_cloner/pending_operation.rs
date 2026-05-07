#![allow(dead_code)]

use std::{collections::HashMap as StdHashMap, sync::Arc, time::Instant};

use scc::HashMap;
use solana_pubkey::Pubkey;
use tokio::sync::{oneshot, watch};

use super::{super::errors::ChainlinkError, types::FetchAndCloneResult};

pub(super) type PendingGeneration = u64;
pub(super) type WaiterId = u64;

pub(super) struct Pending {
    pub generation: PendingGeneration,
    pub deadline: Instant,
    pub waiters: StdHashMap<WaiterId, oneshot::Sender<PendingTerminal>>,
    pub cancel: watch::Sender<bool>,
}

#[derive(Debug, Clone)]
pub(super) enum PendingTerminal {
    Success(FetchAndCloneResult),
    Failed(PendingFailure),
}

#[derive(Debug, Clone)]
pub(super) enum PendingFailure {
    OwnerFailed(String),
    OwnerJoinFailed(String),
    TimedOut,
}

impl PendingFailure {
    pub(super) fn into_chainlink_error(self, pubkey: Pubkey) -> ChainlinkError {
        match self {
            Self::OwnerFailed(msg) => {
                ChainlinkError::PendingRequestOwnerFailed(pubkey, msg)
            }
            Self::OwnerJoinFailed(msg) => {
                ChainlinkError::PendingRequestOwnerDisappeared(pubkey, msg)
            }
            Self::TimedOut => ChainlinkError::PendingRequestTimeout(pubkey),
        }
    }
}

pub(super) struct PendingWaiter {
    pending: Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    waiter_id: WaiterId,
    receiver: oneshot::Receiver<PendingTerminal>,
    completed: bool,
}

impl PendingWaiter {
    fn new(
        pending: Arc<HashMap<Pubkey, Pending>>,
        pubkey: Pubkey,
        generation: PendingGeneration,
        waiter_id: WaiterId,
        receiver: oneshot::Receiver<PendingTerminal>,
    ) -> Self {
        Self {
            pending,
            pubkey,
            generation,
            waiter_id,
            receiver,
            completed: false,
        }
    }

    pub(super) fn pubkey(&self) -> Pubkey {
        self.pubkey
    }

    pub(super) fn generation(&self) -> PendingGeneration {
        self.generation
    }

    pub(super) async fn wait(
        mut self,
    ) -> Result<PendingTerminal, ChainlinkError> {
        let receiver =
            std::mem::replace(&mut self.receiver, oneshot::channel().1);
        match receiver.await {
            Ok(terminal) => {
                self.completed = true;
                Ok(terminal)
            }
            Err(err) => {
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

        self.pending.update(&self.pubkey, |_, pending| {
            if pending.generation == self.generation {
                pending.waiters.remove(&self.waiter_id);
            }
        });
    }
}

pub(super) enum PendingClaim {
    Created(PendingWaiter),
    Joined(PendingWaiter),
}

pub(super) fn claim_or_join_pending(
    pending: Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    waiter_id: WaiterId,
    deadline: Instant,
) -> PendingClaim {
    let (tx, rx) = oneshot::channel();

    let claim = match pending.entry(pubkey) {
        scc::hash_map::Entry::Vacant(entry) => {
            let (cancel, _) = watch::channel(false);
            entry.insert_entry(Pending {
                generation,
                deadline,
                waiters: StdHashMap::from([(waiter_id, tx)]),
                cancel,
            });
            PendingClaim::Created(PendingWaiter::new(
                pending.clone(),
                pubkey,
                generation,
                waiter_id,
                rx,
            ))
        }
        scc::hash_map::Entry::Occupied(mut entry) => {
            let generation = entry.get().generation;
            entry.get_mut().waiters.insert(waiter_id, tx);
            PendingClaim::Joined(PendingWaiter::new(
                pending.clone(),
                pubkey,
                generation,
                waiter_id,
                rx,
            ))
        }
    };

    claim
}

pub(super) fn finish_pending(
    pending: &Arc<HashMap<Pubkey, Pending>>,
    pubkey: Pubkey,
    generation: PendingGeneration,
    terminal: PendingTerminal,
) -> usize {
    if let Some((_, pending)) =
        pending.remove_if(&pubkey, |pending| pending.generation == generation)
    {
        let waiter_count = pending.waiters.len();
        for (_, waiter) in pending.waiters {
            let _ = waiter.send(terminal.clone());
        }
        waiter_count
    } else {
        0
    }
}
