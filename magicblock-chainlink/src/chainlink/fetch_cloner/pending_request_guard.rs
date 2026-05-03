use std::{sync::Arc, time::Instant};

use super::super::errors::ChainlinkError;

use scc::HashMap;
use solana_pubkey::Pubkey;
use tokio::sync::oneshot;
use tracing::warn;

use super::types::FetchAndCloneResult;

pub(super) struct PendingRequestState {
    pub generation: u64,
    pub created_at: Instant,
    pub waiters: Vec<oneshot::Sender<PendingRequestCompletion>>,
}

#[derive(Debug)]
pub(super) enum PendingRequestCompletion {
    Success(FetchAndCloneResult),
    Failed(ChainlinkError),
}

impl Clone for PendingRequestCompletion {
    fn clone(&self) -> Self {
        match self {
            Self::Success(result) => Self::Success(result.clone()),
            Self::Failed(err) => Self::Failed(clone_pending_request_error(err)),
        }
    }
}

fn clone_pending_request_error(err: &ChainlinkError) -> ChainlinkError {
    match err {
        ChainlinkError::PendingRequestTimeout(pubkey) => {
            ChainlinkError::PendingRequestTimeout(*pubkey)
        }
        ChainlinkError::PendingRequestOwnerDropped(pubkey) => {
            ChainlinkError::PendingRequestOwnerDropped(*pubkey)
        }
        ChainlinkError::PendingRequestOwnerDisappeared(pubkey, msg) => {
            ChainlinkError::PendingRequestOwnerDisappeared(*pubkey, msg.clone())
        }
        ChainlinkError::StalePendingRequestEvicted(pubkey, age) => {
            ChainlinkError::StalePendingRequestEvicted(*pubkey, *age)
        }
        ChainlinkError::PendingRequestOwnerFailed(pubkey, msg) => {
            ChainlinkError::PendingRequestOwnerFailed(*pubkey, msg.clone())
        }
        _ => unreachable!("PendingRequestCompletion::Failed must only hold pending-request error variants"),
    }
}

pub(super) enum PendingRequestClaim {
    Owner(PendingRequestGuard),
    Waiter(oneshot::Receiver<PendingRequestCompletion>),
}

pub(super) struct PendingRequestGuard {
    pending_requests: Arc<HashMap<Pubkey, PendingRequestState>>,
    pubkey: Pubkey,
    generation: u64,
    dismissed: bool,
}

impl PendingRequestGuard {
    pub(super) fn new(
        pending_requests: Arc<HashMap<Pubkey, PendingRequestState>>,
        pubkey: Pubkey,
        generation: u64,
    ) -> Self {
        Self {
            pending_requests,
            pubkey,
            generation,
            dismissed: false,
        }
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// Mark the guard as dismissed so `Drop` becomes a no-op.
    pub(super) fn dismiss(&mut self) {
        self.dismissed = true;
    }
}

impl Drop for PendingRequestGuard {
    fn drop(&mut self) {
        if self.dismissed {
            return;
        }
        let waiters = {
            if let Some((_, state)) =
                self.pending_requests.remove_if(&self.pubkey, |state| {
                    state.generation == self.generation
                })
            {
                state.waiters
            } else {
                vec![]
            }
        };
        let waiters_len = waiters.len();
        for tx in waiters {
            let _ = tx.send(PendingRequestCompletion::Failed(
                ChainlinkError::PendingRequestOwnerDropped(self.pubkey),
            ));
        }
        if waiters_len > 0 {
            warn!(
                pubkey = %self.pubkey,
                waiter_count = waiters_len,
                "Cleaning up FetchCloner pending request entry after owner cancellation"
            );
        }
    }
}
