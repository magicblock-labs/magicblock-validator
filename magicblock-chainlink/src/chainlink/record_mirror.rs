use std::{num::NonZeroUsize, sync::Arc};

use lru::LruCache;
use magicblock_config::config::RecordSyncConfig;
use magicblock_metrics::metrics::{self, RecordMirrorLookupOutcome};
use magicblock_sync::{AccountUpdate, DlpSyncer};
use parking_lot::Mutex as PlMutex;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{info, warn};

use crate::{
    chainlink::ObservedUndelegationRequest,
    remote_account_provider::{Endpoint, Endpoints},
};

/// Capacity for pending discovery / undelegation-request events awaiting
/// their chainlink consumers.
const MIRROR_EVENT_CHANNEL_CAPACITY: usize = 4096;

/// A delegation observed on chain, naming the delegated account (which the
/// record PDA alone cannot reveal).
#[derive(Debug, Clone, Copy)]
pub struct DiscoveredDelegation {
    pub delegated_account: Pubkey,
    pub record: Pubkey,
    pub slot: u64,
}

/// In-memory mirror of on-chain delegation records, fed by the
/// magicblock-sync record firehose.
///
/// The firehose carries every delegation-record update on chain plus in-band
/// `SlotAdvanced` watermarks, delivered losslessly. That gives the mirror a
/// freshness proof: an entry that has seen no update while the watermark
/// advanced to slot `w` IS the record's state at every slot up to `w`. On any
/// continuity break (`SyncInterrupted`) the mirror clears itself, so a lookup
/// can only degrade to [`MirrorLookup::Miss`] — and every non-hit outcome
/// falls back to an RPC fetch. Absence here is never "not delegated".
pub struct DelegationRecordMirror {
    inner: PlMutex<MirrorState>,
    /// Delegations observed in the tx stream, consumed by discovery.
    discoveries_tx: Sender<DiscoveredDelegation>,
    discoveries_rx: PlMutex<Option<Receiver<DiscoveredDelegation>>>,
    /// Undelegation requests observed on chain.
    undelegation_requests_tx: Sender<ObservedUndelegationRequest>,
    undelegation_requests_rx:
        PlMutex<Option<Receiver<ObservedUndelegationRequest>>>,
}

struct MirrorState {
    /// Record PDA -> last observed state.
    entries: LruCache<Pubkey, RecordEntry>,
    /// Latest `SlotAdvanced` watermark; 0 until the first one.
    watermark: u64,
    /// True while the stream is live and the watermark trustworthy.
    live: bool,
}

struct RecordEntry {
    slot: u64,
    /// Record bytes, or `None` for an undelegation tombstone.
    data: Option<Vec<u8>>,
}

/// Result of a mirror lookup.
pub enum MirrorLookup {
    /// The record's state at or after the requested slot.
    Hit { data: Vec<u8>, slot: u64 },
    /// The record was observed undelegated. Callers must still confirm via
    /// RPC: undelegation events are heuristic (tx-parsed) in the stream.
    Tombstone { slot: u64 },
    /// Nothing provable for the requested slot.
    Miss,
}

impl DelegationRecordMirror {
    /// Starts the mirror from config, connecting the record firehose.
    ///
    /// Returns `None` (with a warning) when disabled, when no gRPC
    /// endpoint/api-key can be resolved, or when the initial connection
    /// fails: the mirror is an optimization and must never block startup.
    pub async fn try_from_config(
        config: &RecordSyncConfig,
        endpoints: &Endpoints,
    ) -> Option<Arc<Self>> {
        if !config.enabled {
            return None;
        }

        let (endpoint, api_key) = match resolve_endpoint(config, endpoints) {
            Some(resolved) => resolved,
            None => {
                warn!(
                    "record mirror enabled but no gRPC endpoint with api key \
                     available; running without it"
                );
                return None;
            }
        };

        let updates =
            match DlpSyncer::start_firehose(endpoint.clone(), api_key).await {
                Ok(updates) => updates,
                Err(error) => {
                    warn!(
                        ?error,
                        endpoint,
                        "record mirror failed to connect; running without it"
                    );
                    return None;
                }
            };

        info!(endpoint, "record mirror connected");
        Some(Self::start_consumer(updates, config.capacity))
    }

    fn start_consumer(
        mut updates: Receiver<AccountUpdate>,
        capacity: usize,
    ) -> Arc<Self> {
        let mirror = Arc::new(Self::with_capacity(capacity));
        let consumer = Arc::clone(&mirror);
        tokio::spawn(async move {
            while let Some(update) = updates.recv().await {
                consumer.apply(update);
            }
            // Stream task gone for good; leave the mirror cold.
            consumer.clear();
            metrics::set_record_mirror_live(false);
            warn!("record mirror stream ended; running without it");
        });
        mirror
    }

    fn with_capacity(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity)
            .unwrap_or(NonZeroUsize::new(1).expect("1 is non-zero"));
        let (discoveries_tx, discoveries_rx) =
            mpsc::channel(MIRROR_EVENT_CHANNEL_CAPACITY);
        let (undelegation_requests_tx, undelegation_requests_rx) =
            mpsc::channel(MIRROR_EVENT_CHANNEL_CAPACITY);
        Self {
            inner: PlMutex::new(MirrorState {
                entries: LruCache::new(capacity),
                watermark: 0,
                live: false,
            }),
            discoveries_tx,
            discoveries_rx: PlMutex::new(Some(discoveries_rx)),
            undelegation_requests_tx,
            undelegation_requests_rx: PlMutex::new(Some(
                undelegation_requests_rx,
            )),
        }
    }

    /// Takes the discovery event stream; `None` after the first call.
    pub fn take_discoveries(&self) -> Option<Receiver<DiscoveredDelegation>> {
        self.discoveries_rx.lock().take()
    }

    /// Takes the undelegation-request event stream; `None` after the first
    /// call.
    pub fn take_undelegation_requests(
        &self,
    ) -> Option<Receiver<ObservedUndelegationRequest>> {
        self.undelegation_requests_rx.lock().take()
    }

    fn apply(&self, update: AccountUpdate) {
        match update {
            AccountUpdate::Delegated { record, data, slot } => {
                self.insert(Pubkey::new_from_array(record), slot, Some(data));
            }
            AccountUpdate::Undelegated { record, slot } => {
                self.insert(Pubkey::new_from_array(record), slot, None);
            }
            AccountUpdate::DelegationObserved {
                delegated_account,
                record,
                slot,
            } => {
                let discovered = DiscoveredDelegation {
                    delegated_account: Pubkey::new_from_array(
                        delegated_account,
                    ),
                    record: Pubkey::new_from_array(record),
                    slot,
                };
                // Lossy under extreme backpressure by design: a dropped
                // discovery is healed by the on-demand ensure path.
                if self.discoveries_tx.try_send(discovered).is_err() {
                    warn!(
                        delegated_account = %discovered.delegated_account,
                        "discovery channel full; dropping observed delegation"
                    );
                }
            }
            AccountUpdate::UndelegationRequested {
                request_pda,
                delegated_account,
                expires_at_slot,
                slot,
            } => {
                let request = ObservedUndelegationRequest {
                    request_pda: Pubkey::new_from_array(request_pda),
                    delegated_account: Pubkey::new_from_array(
                        delegated_account,
                    ),
                    expires_at_slot,
                    observed_slot: slot,
                };
                let delegated = request.delegated_account;
                if self.undelegation_requests_tx.try_send(request).is_err() {
                    warn!(
                        delegated_account = %delegated,
                        "undelegation-request channel full; dropping (poll backstop covers it)"
                    );
                }
            }
            AccountUpdate::SlotAdvanced(slot) => {
                let mut inner = self.inner.lock();
                inner.watermark = inner.watermark.max(slot);
                inner.live = true;
                let watermark = inner.watermark;
                drop(inner);
                metrics::set_record_mirror_live(true);
                metrics::set_record_mirror_watermark(watermark);
            }
            AccountUpdate::SyncInterrupted | AccountUpdate::SyncTerminated => {
                self.clear();
                metrics::set_record_mirror_live(false);
            }
        }
    }

    /// Inserts monotonically: an update older than the stored slot is a
    /// resume re-delivery and must not roll state back.
    fn insert(&self, record: Pubkey, slot: u64, data: Option<Vec<u8>>) {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.entries.peek(&record) {
            if existing.slot > slot {
                return;
            }
        }
        inner.entries.put(record, RecordEntry { slot, data });
    }

    fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.entries.clear();
        inner.watermark = 0;
        inner.live = false;
    }

    /// Looks up a record's state provable at or after `min_context_slot`.
    ///
    /// An entry qualifies when its own slot reaches `min_context_slot`, or
    /// when the live watermark does: the firehose carries every record
    /// update, so an entry unchanged while the watermark advanced past the
    /// requested slot is that record's current state.
    pub fn get(&self, record: &Pubkey, min_context_slot: u64) -> MirrorLookup {
        let (lookup, non_hit_outcome) = self.lookup(record, min_context_slot);
        // A hit is not counted here: callers validate the bytes (and the
        // pairing slot) first and count Hit/ParseFallback/Stale so the hit
        // metric only reports lookups that actually skipped an RPC.
        if let Some(outcome) = non_hit_outcome {
            metrics::inc_record_mirror_lookup(outcome);
        }
        lookup
    }

    /// Like [`Self::get`] but without lookup metrics: for discovery probes
    /// where a miss is the expected common case and would drown the
    /// RPC-fallback-rate signal the metrics exist to expose.
    pub fn probe(
        &self,
        record: &Pubkey,
        min_context_slot: u64,
    ) -> MirrorLookup {
        self.lookup(record, min_context_slot).0
    }

    fn lookup(
        &self,
        record: &Pubkey,
        min_context_slot: u64,
    ) -> (MirrorLookup, Option<RecordMirrorLookupOutcome>) {
        let mut inner = self.inner.lock();
        let fresh_watermark = inner.live && inner.watermark >= min_context_slot;
        let Some(entry) = inner.entries.get(record) else {
            return (MirrorLookup::Miss, Some(RecordMirrorLookupOutcome::Miss));
        };

        if entry.slot < min_context_slot && !fresh_watermark {
            return (
                MirrorLookup::Miss,
                Some(RecordMirrorLookupOutcome::Stale),
            );
        }

        match &entry.data {
            Some(data) => (
                MirrorLookup::Hit {
                    data: data.clone(),
                    slot: entry.slot,
                },
                None,
            ),
            None => (
                MirrorLookup::Tombstone { slot: entry.slot },
                Some(RecordMirrorLookupOutcome::Tombstone),
            ),
        }
    }

    /// Drops a single entry, forcing the next lookup to fall back to RPC.
    pub fn invalidate(&self, record: &Pubkey) {
        self.inner.lock().entries.pop(record);
    }
}

/// Resolves the firehose endpoint. Each field independently prefers the
/// explicit config value and falls back to the first gRPC remote (which
/// always carries an api key), so partial overrides take effect.
fn resolve_endpoint(
    config: &RecordSyncConfig,
    endpoints: &Endpoints,
) -> Option<(String, String)> {
    let grpc_remote = endpoints.iter().find_map(|ep| match ep {
        Endpoint::Grpc { url, api_key, .. } => {
            Some((url.clone(), api_key.clone()))
        }
        _ => None,
    });

    let endpoint = config
        .endpoint
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| grpc_remote.as_ref().map(|(url, _)| url.clone()))?;
    let api_key = config
        .api_key
        .clone()
        .or_else(|| grpc_remote.map(|(_, key)| key))?;

    Some((endpoint, api_key))
}

#[cfg(any(test, feature = "dev-context"))]
impl DelegationRecordMirror {
    pub fn new_for_tests() -> Arc<Self> {
        Arc::new(Self::with_capacity(1024))
    }

    pub fn test_insert_record(&self, record: Pubkey, data: Vec<u8>, slot: u64) {
        self.insert(record, slot, Some(data));
    }

    pub fn test_insert_tombstone(&self, record: Pubkey, slot: u64) {
        self.insert(record, slot, None);
    }

    pub fn test_set_watermark(&self, slot: u64) {
        self.apply(AccountUpdate::SlotAdvanced(slot));
    }

    pub fn test_clear(&self) {
        self.clear();
    }

    /// Drives a raw sync event through the consumer path.
    pub fn test_apply(&self, update: AccountUpdate) {
        self.apply(update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Pubkey {
        Pubkey::new_unique()
    }

    #[test]
    fn hit_requires_entry_slot_or_live_watermark() {
        let mirror = DelegationRecordMirror::with_capacity(16);
        let pda = record();
        mirror.insert(pda, 50, Some(vec![1]));

        // Entry slot below the requested slot, no watermark -> miss.
        assert!(matches!(mirror.get(&pda, 100), MirrorLookup::Miss));
        // Entry slot satisfies the request directly.
        assert!(matches!(mirror.get(&pda, 40), MirrorLookup::Hit { .. }));

        // Watermark past the requested slot proves the entry unchanged.
        mirror.apply(AccountUpdate::SlotAdvanced(120));
        assert!(matches!(
            mirror.get(&pda, 100),
            MirrorLookup::Hit { slot: 50, .. }
        ));
        // But not beyond the watermark.
        assert!(matches!(mirror.get(&pda, 121), MirrorLookup::Miss));
    }

    #[test]
    fn inserts_are_monotonic_per_record() {
        let mirror = DelegationRecordMirror::with_capacity(16);
        let pda = record();
        mirror.insert(pda, 50, Some(vec![1]));
        mirror.insert(pda, 40, Some(vec![9]));

        let MirrorLookup::Hit { data, slot } = mirror.get(&pda, 40) else {
            panic!("expected hit");
        };
        assert_eq!((data.as_slice(), slot), (&[1][..], 50));

        // A newer tombstone replaces the record.
        mirror.insert(pda, 60, None);
        assert!(matches!(
            mirror.get(&pda, 40),
            MirrorLookup::Tombstone { slot: 60 }
        ));
    }

    #[test]
    fn interruption_clears_entries_and_watermark() {
        let mirror = DelegationRecordMirror::with_capacity(16);
        let pda = record();
        mirror.insert(pda, 50, Some(vec![1]));
        mirror.apply(AccountUpdate::SlotAdvanced(120));

        mirror.apply(AccountUpdate::SyncInterrupted);
        assert!(matches!(mirror.get(&pda, 1), MirrorLookup::Miss));

        // A stale watermark from before the gap must not count.
        mirror.insert(pda, 130, Some(vec![2]));
        assert!(matches!(mirror.get(&pda, 140), MirrorLookup::Miss));
    }

    #[test]
    fn lru_eviction_degrades_to_miss() {
        let mirror = DelegationRecordMirror::with_capacity(2);
        let (a, b, c) = (record(), record(), record());
        mirror.insert(a, 10, Some(vec![1]));
        mirror.insert(b, 11, Some(vec![2]));
        mirror.insert(c, 12, Some(vec![3]));

        assert!(matches!(mirror.get(&a, 5), MirrorLookup::Miss));
        assert!(matches!(mirror.get(&b, 5), MirrorLookup::Hit { .. }));
        assert!(matches!(mirror.get(&c, 5), MirrorLookup::Hit { .. }));
    }
}
