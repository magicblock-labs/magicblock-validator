use std::{sync::Arc, time::Instant};

use scc::HashMap;
use solana_pubkey::Pubkey;
use tokio::sync::oneshot;
use tracing::warn;

pub(super) struct PendingRequestState {
    pub created_at: Instant,
    pub waiters: Vec<oneshot::Sender<PendingRequestCompletion>>,
}

#[derive(Debug, Clone)]
pub(super) enum PendingRequestCompletion {
    Success,
    Failed(String),
}

pub(super) enum PendingRequestClaim {
    Owner(PendingRequestGuard),
    Waiter(oneshot::Receiver<PendingRequestCompletion>),
}

pub(super) struct PendingRequestGuard {
    pending_requests: Arc<HashMap<Pubkey, PendingRequestState>>,
    pubkey: Pubkey,
    dismissed: bool,
}

impl PendingRequestGuard {
    pub(super) fn new(
        pending_requests: Arc<HashMap<Pubkey, PendingRequestState>>,
        pubkey: Pubkey,
    ) -> Self {
        Self {
            pending_requests,
            pubkey,
            dismissed: false,
        }
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
            if let Some((_, state)) = self.pending_requests.remove(&self.pubkey)
            {
                state.waiters
            } else {
                vec![]
            }
        };
        let waiters_len = waiters.len();
        for tx in waiters {
            let _ = tx.send(PendingRequestCompletion::Failed(
                "owner future dropped before cleanup".to_string(),
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
