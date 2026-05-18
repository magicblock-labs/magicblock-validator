use std::{
    collections::hash_map,
    sync::{Arc, Mutex},
};

use solana_pubkey::Pubkey;
use tokio::sync::oneshot;

pub(super) type CloneKey = (Pubkey, u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloneCompletion {
    Success,
    Failed,
}

pub(super) enum CloneClaim {
    Owner,
    Waiter(oneshot::Receiver<CloneCompletion>),
}

/// Drop guard that calls `finish_pending_clone` with `Failed` unless
/// explicitly dismissed. Protects against task cancellation or panics
/// leaving waiters hanging forever.
pub(super) struct PendingCloneGuard {
    pending_clones: Arc<
        Mutex<
            hash_map::HashMap<CloneKey, Vec<oneshot::Sender<CloneCompletion>>>,
        >,
    >,
    key: CloneKey,
    dismissed: bool,
}

impl PendingCloneGuard {
    pub(super) fn new(
        pending_clones: Arc<
            Mutex<
                hash_map::HashMap<
                    CloneKey,
                    Vec<oneshot::Sender<CloneCompletion>>,
                >,
            >,
        >,
        pubkey: Pubkey,
        slot: u64,
    ) -> Self {
        Self {
            pending_clones,
            key: (pubkey, slot),
            dismissed: false,
        }
    }

    /// Mark the guard as dismissed so `Drop` becomes a no-op.
    pub(super) fn dismiss(&mut self) {
        self.dismissed = true;
    }
}

impl Drop for PendingCloneGuard {
    fn drop(&mut self) {
        if self.dismissed {
            return;
        }
        let waiters = {
            let mut map = self
                .pending_clones
                .lock()
                .expect("pending_clones mutex poisoned");
            map.remove(&self.key).unwrap_or_default()
        };
        for tx in waiters {
            let _ = tx.send(CloneCompletion::Failed);
        }
    }
}
