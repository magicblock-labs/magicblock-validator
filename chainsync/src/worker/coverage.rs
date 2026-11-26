use std::collections::hash_map::Entry::Occupied;

use ahash::AHashMap;
use solana_pubkey::Pubkey;

use crate::{AccountSubscription, metrics};

/// Update origin, including the exact gRPC stream authorized to supply it.
#[derive(Clone, Copy)]
pub(super) enum UpdateSource {
    WebSocket,
    Grpc(usize),
}

/// Admission evidence rechecked after waiting for an Engine lease.
#[derive(Clone, Copy)]
pub(super) struct Ticket {
    pub(super) source: UpdateSource,
    pub(super) sub: AccountSubscription,
    generation: u64,
}

/// Transport that supplied or lost an account subscription.
pub(super) use crate::metrics::Transport as Source;

/// Tracks which transports currently cover one account subscription.
struct Entry {
    /// Remote address and the local account its updates belong to.
    sub: AccountSubscription,
    /// Distinguishes a current subscription from late transport confirmations.
    generation: u64,
    /// Whether WebSocket subscription acknowledgement was received and the source is still active.
    ws: bool,
    /// gRPC stream that sent this account's filter, if it has not since reported loss.
    grpc: Option<usize>,
}

impl Entry {
    /// Metric label for the active transports; `None` when neither remains.
    fn coverage(&self) -> Option<&'static str> {
        match (self.ws, self.grpc.is_some()) {
            (true, false) => Some("ws_only"),
            (false, true) => Some("grpc_only"),
            (true, true) => Some("both"),
            (false, false) => None,
        }
    }
}

/// Active account subscriptions, updated only by the ChainSync event worker.
#[derive(Default)]
pub(super) struct Coverage {
    /// Remote addresses covered by at least one active transport.
    entries: AHashMap<Pubkey, Entry>,
    /// Counter distinguishing new subscriptions from delayed confirmations of removed ones.
    generation: u64,
}

impl Coverage {
    /// Records a WebSocket acknowledgement and returns the generation to attach to gRPC commands.
    pub(super) fn acknowledged(&mut self, sub: AccountSubscription) -> u64 {
        if let Some(entry) = self.entries.get_mut(&sub.pubkey) {
            let before = entry.coverage();
            entry.ws = true;
            metrics::coverage(before, entry.coverage());
            return entry.generation;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let entry = Entry {
            sub,
            generation,
            ws: true,
            grpc: None,
        };
        metrics::coverage(None, entry.coverage());
        self.entries.insert(sub.pubkey, entry);
        generation
    }

    /// Revokes remote coverage, optionally requiring an exact subscription match.
    pub(super) fn revoke(
        &mut self,
        pubkey: Pubkey,
        expected: Option<AccountSubscription>,
    ) -> Option<AccountSubscription> {
        let Occupied(entry) = self.entries.entry(pubkey) else { return None };
        if expected.is_some_and(|sub| entry.get().sub != sub) {
            return None;
        }
        let entry = entry.remove();
        metrics::coverage(entry.coverage(), None);
        Some(entry.sub)
    }

    /// Whether any current direct or ProgramData subscription feeds this local account.
    pub(super) fn covers(&self, local_pubkey: Pubkey) -> bool {
        AccountSubscription::for_account(local_pubkey)
            .into_iter()
            .any(|sub| self.entries.get(&sub.pubkey).is_some_and(|entry| entry.sub == sub))
    }

    /// Drops one transport for a remote address; returns whether that address lost all coverage
    /// and the local address to evict, if no other subscription feeds it.
    pub(super) fn lost(&mut self, source: Source, pubkey: Pubkey) -> (bool, Option<Pubkey>) {
        let Some(entry) = self.entries.get_mut(&pubkey) else { return (false, None) };
        let before = entry.coverage();
        match source {
            Source::WebSocket => entry.ws = false,
            Source::Grpc => entry.grpc = None,
        }
        metrics::coverage(before, entry.coverage());
        // Losing WebSocket alone does not evict an account with a confirmed gRPC copy.
        if entry.ws || entry.grpc.is_some() {
            return (false, None);
        }
        metrics::lost(source);
        let local = self.revoke(pubkey, None).map(AccountSubscription::local_pubkey);
        (true, local.filter(|&pubkey| !self.covers(pubkey)))
    }

    /// Records a sent gRPC filter only if its generation still matches the current subscription.
    pub(super) fn confirmed(&mut self, stream: usize, pubkey: Pubkey, generation: u64) {
        let Some(entry) = self.entries.get_mut(&pubkey) else { return };
        if entry.generation == generation {
            let before = entry.coverage();
            entry.grpc = Some(stream);
            metrics::coverage(before, entry.coverage());
        }
    }

    /// Accepts an update only if this exact subscription still has WebSocket coverage.
    #[cfg(test)]
    pub(super) fn ws_contains(&self, sub: AccountSubscription) -> bool {
        self.ticket(UpdateSource::WebSocket, sub).is_some()
    }

    /// Accepts an update only if this exact subscription is still covered by the sending gRPC stream.
    #[cfg(test)]
    pub(super) fn grpc_contains(&self, stream: usize, sub: AccountSubscription) -> bool {
        self.ticket(UpdateSource::Grpc(stream), sub).is_some()
    }

    /// Captures coverage without allowing a later reacquisition to authorize this update.
    pub(super) fn ticket(&self, source: UpdateSource, sub: AccountSubscription) -> Option<Ticket> {
        let entry = self.entries.get(&sub.pubkey)?;
        let covered = match source {
            UpdateSource::WebSocket => entry.ws,
            UpdateSource::Grpc(stream) => entry.grpc == Some(stream),
        };
        (covered && entry.sub == sub).then_some(Ticket {
            source,
            sub,
            generation: entry.generation,
        })
    }

    pub(super) fn authorizes(&self, ticket: Ticket) -> bool {
        self.ticket(ticket.source, ticket.sub)
            .is_some_and(|current| current.generation == ticket.generation)
    }

    /// Lists keys covered by a failed gRPC stream before removing that source.
    pub(super) fn stream_keys(&self, stream: usize) -> Vec<Pubkey> {
        self.entries
            .iter()
            .filter_map(|(&pubkey, entry)| (entry.grpc == Some(stream)).then_some(pubkey))
            .collect()
    }

    /// Removes coverage only if the stored subscription matches; returns whether it was removed.
    /// Used during cache eviction, so no further Engine eviction is requested.
    pub(super) fn remove(&mut self, sub: AccountSubscription) -> bool {
        self.revoke(sub.pubkey, Some(sub)).is_some()
    }
}

impl Drop for Coverage {
    /// Removes this worker's surviving subscriptions from process-wide coverage gauges.
    fn drop(&mut self) {
        for entry in self.entries.values() {
            metrics::coverage(entry.coverage(), None);
        }
    }
}
