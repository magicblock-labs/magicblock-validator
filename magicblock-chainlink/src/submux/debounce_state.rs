use std::{collections::VecDeque, time::Instant};

use solana_pubkey::Pubkey;

use crate::remote_account_provider::SubscriptionUpdate;

/// Per-account debounce tracking state used by SubMuxClient.
/// Maintains a small sliding-window history and scheduling info so
/// high-frequency updates are coalesced into at most one update per
/// debounce interval, always sending the most recent payload.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum DebounceState {
    /// Debouncing is currently disabled for this pubkey.
    /// We still track recent arrival timestamps to determine
    /// when to enable debouncing if the rate increases.
    Disabled {
        /// The pubkey this state applies to.
        pubkey: Pubkey,
        /// Arrival timestamps (Instant) of recent updates within the
        /// detection window. Old entries are pruned as time advances.
        arrivals: VecDeque<Instant>,
    },
    /// Debouncing is enabled: high-frequency updates will be
    /// coalesced so that at most one update is forwarded per
    /// debounce interval. The most recent pending update is
    /// always the one forwarded when the interval elapses.
    Enabled {
        /// The pubkey this state applies to.
        pubkey: Pubkey,
        /// Arrival timestamps (Instant) of recent updates within the
        /// detection window. Old entries are pruned as time advances.
        arrivals: VecDeque<Instant>,
        /// Earliest Instant at which we are allowed to forward the
        /// next update for this pubkey.
        next_allowed_forward: Instant,
        /// Latest update observed while waiting for next_allowed_forward.
        /// Replaced on subsequent arrivals to ensure we forward the
        /// freshest state.
        pending: Option<SubscriptionUpdate>,
    },
}

impl DebounceState {
    /// If currently Disabled, transition to Enabled and initialize
    /// scheduling fields. Returns true if state changed.
    pub fn maybe_enable(&mut self, now: Instant) -> bool {
        if let DebounceState::Disabled {
            ref mut arrivals,
            pubkey: ref pk,
        } = self
        {
            let a = std::mem::take(arrivals);
            let pubkey = *pk;
            *self = DebounceState::Enabled {
                pubkey,
                arrivals: a,
                next_allowed_forward: now,
                pending: None,
            };
            true
        } else {
            false
        }
    }

    /// If currently Enabled, transition to Disabled while preserving
    /// arrival history. Returns true if state changed.
    pub fn maybe_disable(&mut self) -> bool {
        if let DebounceState::Enabled {
            ref mut arrivals,
            pubkey: ref pk,
            ..
        } = self
        {
            let a = std::mem::take(arrivals);
            let pubkey = *pk;
            *self = DebounceState::Disabled {
                pubkey,
                arrivals: a,
            };
            true
        } else {
            false
        }
    }

    /// Get a mutable reference to the arrivals VecDeque regardless of state.
    pub fn arrivals_mut(&mut self) -> &mut VecDeque<Instant> {
        use DebounceState::*;
        match self {
            Disabled { arrivals, .. } => arrivals,
            Enabled { arrivals, .. } => arrivals,
        }
    }

    /// Get an immutable reference to the arrivals VecDeque regardless of state.
    pub fn arrivals_ref(&self) -> &VecDeque<Instant> {
        use DebounceState::*;
        match self {
            Disabled { arrivals, .. } => arrivals,
            Enabled { arrivals, .. } => arrivals,
        }
    }

    /// The ms in between arrivals in the sliding window.
    pub fn arrival_deltas_ms(&self) -> Vec<u64> {
        let arrivals = self.arrivals_ref();
        let mut deltas = Vec::new();
        if arrivals.len() < 2 {
            return deltas;
        }
        let mut prev = arrivals[0];
        for &curr in arrivals.iter().skip(1) {
            let delta = curr.saturating_duration_since(prev).as_millis() as u64;
            deltas.push(delta);
            prev = curr;
        }
        deltas
    }

    pub fn label(&self) -> &str {
        use DebounceState::*;
        match self {
            Disabled { .. } => "Disabled",
            Enabled { .. } => "Enabled",
        }
    }
}
