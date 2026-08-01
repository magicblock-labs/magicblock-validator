//! Pending-request and owned-operation bookkeeping.

use std::{
    collections::{hash_map, HashSet},
    sync::atomic::Ordering,
    time::Duration,
};

use magicblock_accounts_db::traits::AccountsBank;
use magicblock_metrics::metrics::{
    AccountFetchContext, ChainlinkPendingFetchLayer,
    ChainlinkPendingFetchOutcome,
};
use solana_pubkey::Pubkey;
use tokio::{sync::oneshot, task};

use super::*;

impl<T, U, V, C> FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    #[cfg(test)]
    pub(super) fn has_pending_request(&self, pubkey: &Pubkey) -> bool {
        self.pending_requests.contains(pubkey)
    }

    #[cfg(test)]
    pub(super) fn set_pending_operation_timeout(&self, timeout: Duration) {
        self.pending_operation_timeout_ms
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }

    /// Returns the number of waiters currently registered for the pending
    /// fetch+clone request keyed by `pubkey`, or `None` if no pending
    /// request exists for that pubkey. Used by tests to deterministically
    /// observe waiter registration without relying on fixed sleeps.
    #[cfg(any(test, feature = "dev-context"))]
    pub fn pending_request_waiter_count(
        &self,
        pubkey: &Pubkey,
    ) -> Option<usize> {
        self.pending_requests
            .read(pubkey, |_, state| state.waiters.len())
    }

    /// Returns the number of waiters currently joined to the low-level
    /// clone operation keyed by `pubkey`, or `None` if no clone is pending.
    #[cfg(any(test, feature = "dev-context"))]
    pub fn pending_clone_waiter_count(&self, pubkey: &Pubkey) -> Option<usize> {
        let map = self
            .pending_clones
            .lock()
            .expect("pending_clones mutex poisoned");
        map.get(pubkey).map(Vec::len)
    }

    /// Cancels the in-flight fetch+clone owner for `pubkey`, if one exists.
    pub fn cancel_pending(&self, pubkey: &Pubkey) {
        self.pending_requests
            .read(pubkey, |_, pending| pending.cancel.notify_one());
    }

    /// Cancels all in-flight fetch+clone owners.
    pub fn cancel_all_pending(&self) {
        self.pending_requests
            .scan(|_pubkey, pending| pending.cancel.notify_one());
    }

    /// Check if a program is allowed to be cloned.
    /// Returns true if:
    /// - No allowed_programs restriction is set (None), OR
    /// - The allowed_programs set is empty (treats empty as unrestricted), OR
    /// - The program is in the allowed_programs set
    pub(super) fn is_program_allowed(&self, program_id: &Pubkey) -> bool {
        match &self.allowed_programs {
            None => true,
            Some(allowed) => {
                if allowed.is_empty() {
                    true
                } else {
                    allowed.contains(program_id)
                }
            }
        }
    }

    /// Attempt to claim ownership of a clone operation for a clone key.
    /// Returns `CloneClaim::Owner` if this caller is the first and should
    /// perform the clone. Returns `CloneClaim::Waiter(rx)` if another
    /// caller already owns this clone and this caller should wait.
    pub(super) fn claim_pending_clone(&self, pubkey: Pubkey) -> CloneClaim {
        let mut map = self
            .pending_clones
            .lock()
            .expect("pending_clones mutex poisoned");
        match map.entry(pubkey) {
            hash_map::Entry::Vacant(entry) => {
                entry.insert(Vec::new());
                CloneClaim::Owner
            }
            hash_map::Entry::Occupied(mut entry) => {
                let (tx, rx) = oneshot::channel();
                entry.get_mut().push(tx);
                CloneClaim::Waiter(rx)
            }
        }
    }

    /// Called by the owner when the clone operation completes.
    /// Removes the pending entry and notifies all waiters.
    pub(super) fn finish_pending_clone(
        &self,
        pubkey: Pubkey,
        result: CloneCompletion,
    ) {
        let waiters = {
            let mut map = self
                .pending_clones
                .lock()
                .expect("pending_clones mutex poisoned");
            map.remove(&pubkey).unwrap_or_default()
        };
        for tx in waiters {
            let _ = tx.send(result);
        }
    }

    pub(super) fn claim_or_join_owned_operation(
        &self,
        pubkey: Pubkey,
        fetch_context: AccountFetchContext,
    ) -> PendingClaim {
        let generation = self.next_pending_request_generation();
        let waiter_id = self.next_pending_waiter_id();
        claim_or_join_pending(
            self.pending_requests.clone(),
            pubkey,
            generation,
            waiter_id,
            Duration::from_millis(
                self.pending_operation_timeout_ms.load(Ordering::Relaxed),
            ),
            fetch_context,
            ChainlinkPendingFetchLayer::FetchCloner,
        )
    }

    // Fetches every pubkey claimed by one dedup call in a single batched wire
    // call, then spawns a per-key clone for each.
    pub(super) fn spawn_batched_owned_operation(
        &self,
        claimed: Vec<ClaimedOperation>,
        mark_empty_set: &HashSet<Pubkey>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) {
        if claimed.is_empty() {
            return;
        }
        let this = self.clone();
        let pending = self.pending_requests.clone();
        let mark_empty = claimed
            .iter()
            .map(|op| op.pubkey)
            .filter(|pubkey| mark_empty_set.contains(pubkey))
            .collect::<HashSet<_>>();
        task::spawn(async move {
            let pubkeys =
                claimed.iter().map(|op| op.pubkey).collect::<Vec<_>>();
            let mark_empty_list =
                mark_empty.iter().copied().collect::<Vec<_>>();
            let mark_empty_ref = (!mark_empty_list.is_empty())
                .then_some(mark_empty_list.as_slice());

            // One shared wire call for the whole claim set, bounded by the
            // earliest claim deadline so a hung fetch cannot leave the
            // waiters of these operations blocked past their timeout.
            let fetch_deadline = claimed
                .iter()
                .map(|op| op.deadline)
                .min()
                .expect("claimed is non-empty");
            let fetch_result = match tokio::time::timeout_at(
                fetch_deadline,
                this.fetch_accounts(
                    &pubkeys,
                    mark_empty_ref,
                    slot,
                    fetch_context.clone(),
                ),
            )
            .await
            {
                Ok(result) => result.map_err(|err| err.to_string()),
                Err(_) => Err("account fetch deadline exceeded".to_string()),
            };
            let accs = match fetch_result {
                Ok(accs) => accs,
                Err(owner_msg) => {
                    let now = tokio::time::Instant::now();
                    for mut op in claimed {
                        let failure = if op.deadline <= now {
                            PendingFailure::TimedOut
                        } else {
                            PendingFailure::OwnerFailed(owner_msg.clone())
                        };
                        op.owner.finish(match failure {
                            PendingFailure::Cancelled => {
                                ChainlinkPendingFetchOutcome::OwnerCancelled
                            }
                            PendingFailure::OwnerFailed(_)
                            | PendingFailure::TimedOut => {
                                ChainlinkPendingFetchOutcome::OwnerFailed
                            }
                        });
                        finish_pending(
                            &pending,
                            op.pubkey,
                            op.generation,
                            PendingTerminal::Failed(failure),
                        );
                    }
                    return;
                }
            };
            // Clone each key on its own so a cancel drops only that key's
            // clone; the fetched account is handed over, never re-fetched.
            for (op, account) in claimed.into_iter().zip(accs) {
                let mark_empty_if_not_found = mark_empty.contains(&op.pubkey);
                this.spawn_owned_operation(
                    op,
                    account,
                    mark_empty_if_not_found,
                    slot,
                    fetch_context.clone(),
                );
            }
        });
    }

    pub(super) fn pending_terminal_owner_outcome(
        terminal: &PendingTerminal,
    ) -> ChainlinkPendingFetchOutcome {
        match terminal {
            PendingTerminal::Success(_) => {
                ChainlinkPendingFetchOutcome::OwnerSucceeded
            }
            PendingTerminal::Failed(PendingFailure::Cancelled) => {
                ChainlinkPendingFetchOutcome::OwnerCancelled
            }
            PendingTerminal::Failed(_) => {
                ChainlinkPendingFetchOutcome::OwnerFailed
            }
        }
    }

    pub(super) fn spawn_owned_operation(
        &self,
        op: ClaimedOperation,
        account: RemoteAccount,
        mark_empty_if_not_found: bool,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) {
        let this = self.clone();
        let pending = self.pending_requests.clone();
        task::spawn(async move {
            let ClaimedOperation {
                pubkey,
                generation,
                deadline,
                cancel,
                mut owner,
            } = op;
            let pubkeys = [pubkey];
            let mark_empty = mark_empty_if_not_found.then_some(vec![pubkey]);
            let mark_empty_ref = mark_empty.as_deref();
            let work = this.clone_accounts(
                &pubkeys,
                vec![account],
                mark_empty_ref,
                slot,
                fetch_context,
            );
            let terminal = tokio::select! {
                biased;

                result = tokio::time::timeout_at(deadline, work) => {
                    match result {
                        Ok(Ok(result)) => PendingTerminal::Success(result),
                        Ok(Err(err)) => PendingTerminal::Failed(
                            PendingFailure::OwnerFailed(err.to_string()),
                        ),
                        Err(_) => PendingTerminal::Failed(PendingFailure::TimedOut),
                    }
                }
                _ = cancel.notified() => {
                    PendingTerminal::Failed(PendingFailure::Cancelled)
                }
            };
            let outcome = Self::pending_terminal_owner_outcome(&terminal);
            owner.finish(outcome);
            finish_pending(&pending, pubkey, generation, terminal);
        });
    }

    pub(super) fn next_pending_request_generation(&self) -> u64 {
        self.pending_request_generation
            .fetch_add(1, Ordering::Relaxed)
    }

    pub(super) fn next_pending_waiter_id(&self) -> u64 {
        self.pending_waiter_generation
            .fetch_add(1, Ordering::Relaxed)
    }
}
