use std::{
    collections::hash_map,
    sync::{Arc, Mutex},
};

use solana_pubkey::Pubkey;
use tokio::sync::oneshot;

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
        Mutex<hash_map::HashMap<Pubkey, Vec<oneshot::Sender<CloneCompletion>>>>,
    >,
    key: Pubkey,
    dismissed: bool,
}

impl PendingCloneGuard {
    pub(super) fn new(
        pending_clones: Arc<
            Mutex<
                hash_map::HashMap<
                    Pubkey,
                    Vec<oneshot::Sender<CloneCompletion>>,
                >,
            >,
        >,
        pubkey: Pubkey,
    ) -> Self {
        Self {
            pending_clones,
            key: pubkey,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drop_fails_waiters_and_releases_claim() {
        let key = Pubkey::new_unique();
        let (tx, rx) = oneshot::channel();
        let pending =
            Arc::new(Mutex::new(hash_map::HashMap::from([(key, vec![tx])])));

        drop(PendingCloneGuard::new(pending.clone(), key));

        assert_eq!(rx.await.unwrap(), CloneCompletion::Failed);
        assert!(!pending.lock().unwrap().contains_key(&key));
    }

    #[test]
    fn dismissed_guard_leaves_completion_to_owner() {
        let key = Pubkey::new_unique();
        let pending =
            Arc::new(Mutex::new(hash_map::HashMap::from([(key, Vec::new())])));
        let mut guard = PendingCloneGuard::new(pending.clone(), key);

        guard.dismiss();
        drop(guard);

        assert!(pending.lock().unwrap().contains_key(&key));
    }
}
