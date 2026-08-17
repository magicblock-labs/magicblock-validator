use std::{num::NonZeroUsize, sync::Arc};

use lru::LruCache;
use magicblock_config::config::RecordSyncConfig;
use magicblock_metrics::metrics::{self, RecordMirrorLookupOutcome};
use parking_lot::Mutex;
use solana_pubkey::Pubkey;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tracing::{info, warn};

use crate::{
    chainlink::{
        ObservedUndelegationRequest,
        record_stream::{RecordStream, RecordStreamUpdate},
    },
    remote_account_provider::{Endpoint, Endpoints},
};

const MIRROR_EVENT_CHANNEL_CAPACITY: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub struct DiscoveredDelegation {
    pub delegated_account: Pubkey,
    pub record: Pubkey,
    pub slot: u64,
}

/// Bounded in-memory mirror of confirmed on-chain delegation records.
///
/// A live stream watermark proves unchanged entries remain current through
/// that slot. Any miss, stale entry, tombstone, malformed value, or continuity
/// loss falls back to the slot-matched RPC path; absence is never interpreted
/// as "not delegated".
pub struct DelegationRecordMirror {
    inner: Mutex<MirrorState>,
    discoveries_tx: Sender<DiscoveredDelegation>,
    discoveries_rx: Mutex<Option<Receiver<DiscoveredDelegation>>>,
    undelegation_requests_tx: Sender<ObservedUndelegationRequest>,
    undelegation_requests_rx:
        Mutex<Option<Receiver<ObservedUndelegationRequest>>>,
}

struct MirrorState {
    entries: LruCache<Pubkey, RecordEntry>,
    watermark: u64,
    live: bool,
}

struct RecordEntry {
    slot: u64,
    data: Option<Vec<u8>>,
}

pub enum MirrorLookup {
    Hit { data: Vec<u8>, slot: u64 },
    Tombstone { slot: u64 },
    Miss,
}

impl DelegationRecordMirror {
    /// Starts the mirror when configured and a gRPC endpoint is available.
    /// Initial stream failure degrades to the existing RPC and DLP-program
    /// subscription paths; it never blocks validator startup.
    pub async fn try_from_config(
        config: &RecordSyncConfig,
        endpoints: &Endpoints,
    ) -> Option<Arc<Self>> {
        if !config.enabled {
            return None;
        }
        let (endpoint, api_key) = match resolve_endpoint(config, endpoints) {
            Some(endpoint) => endpoint,
            None => {
                warn!(
                    "record mirror enabled without a gRPC endpoint and API key"
                );
                return None;
            }
        };
        let updates = match RecordStream::start(endpoint.clone(), api_key).await
        {
            Ok(updates) => updates,
            Err(error) => {
                warn!(
                    ?error,
                    "record stream failed to connect; using RPC fallback"
                );
                return None;
            }
        };
        info!("record mirror connected");
        Some(Self::start_consumer(updates, config.capacity))
    }

    fn start_consumer(
        mut updates: Receiver<RecordStreamUpdate>,
        capacity: usize,
    ) -> Arc<Self> {
        let mirror = Arc::new(Self::with_capacity(capacity));
        let consumer = Arc::clone(&mirror);
        tokio::spawn(async move {
            while let Some(update) = updates.recv().await {
                consumer.consume(update).await;
            }
            consumer.clear();
            metrics::set_record_mirror_live(false);
            warn!("record mirror stream ended; using RPC fallback");
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
            inner: Mutex::new(MirrorState {
                entries: LruCache::new(capacity),
                watermark: 0,
                live: false,
            }),
            discoveries_tx,
            discoveries_rx: Mutex::new(Some(discoveries_rx)),
            undelegation_requests_tx,
            undelegation_requests_rx: Mutex::new(Some(
                undelegation_requests_rx,
            )),
        }
    }

    pub fn take_discoveries(&self) -> Option<Receiver<DiscoveredDelegation>> {
        self.discoveries_rx.lock().take()
    }

    pub fn take_undelegation_requests(
        &self,
    ) -> Option<Receiver<ObservedUndelegationRequest>> {
        self.undelegation_requests_rx.lock().take()
    }

    async fn consume(&self, update: RecordStreamUpdate) {
        match update {
            RecordStreamUpdate::DelegationObserved {
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
                if self.discoveries_tx.send(discovered).await.is_err() {
                    warn!(
                        delegated_account = %discovered.delegated_account,
                        "delegation discovery receiver closed"
                    );
                }
            }
            RecordStreamUpdate::UndelegationRequested {
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
                let delegated_account = request.delegated_account;
                if self.undelegation_requests_tx.send(request).await.is_err() {
                    warn!(
                        %delegated_account,
                        "undelegation request receiver closed; poll backstop remains active"
                    );
                }
            }
            update => self.apply(update),
        }
    }

    fn apply(&self, update: RecordStreamUpdate) {
        match update {
            RecordStreamUpdate::Record { record, data, slot } => {
                self.insert(Pubkey::new_from_array(record), slot, Some(data));
            }
            RecordStreamUpdate::RecordUndelegated { record, slot } => {
                self.insert(Pubkey::new_from_array(record), slot, None);
            }
            RecordStreamUpdate::DelegationObserved { .. }
            | RecordStreamUpdate::UndelegationRequested { .. } => {
                unreachable!("stream events are forwarded by consume")
            }
            RecordStreamUpdate::SlotAdvanced(slot) => {
                let mut inner = self.inner.lock();
                inner.watermark = inner.watermark.max(slot);
                inner.live = true;
                let watermark = inner.watermark;
                drop(inner);
                metrics::set_record_mirror_live(true);
                metrics::set_record_mirror_watermark(watermark);
            }
            RecordStreamUpdate::SyncInterrupted
            | RecordStreamUpdate::SyncTerminated => {
                self.clear();
                metrics::set_record_mirror_live(false);
            }
        }
    }

    fn insert(&self, record: Pubkey, slot: u64, data: Option<Vec<u8>>) {
        let mut inner = self.inner.lock();
        if let Some(existing) = inner.entries.peek(&record) {
            if existing.slot > slot {
                return;
            }
            if existing.slot == slot
                && existing.data.as_deref() != data.as_deref()
            {
                warn!(%record, slot, "conflicting same-slot record updates; requiring RPC confirmation");
                inner.entries.put(record, RecordEntry { slot, data: None });
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

    pub fn get(&self, record: &Pubkey, min_context_slot: u64) -> MirrorLookup {
        let (lookup, outcome) = self.lookup(record, min_context_slot);
        if let Some(outcome) = outcome {
            metrics::inc_record_mirror_lookup(outcome);
        }
        lookup
    }

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
        let watermark_is_fresh =
            inner.live && inner.watermark >= min_context_slot;
        let Some(entry) = inner.entries.get(record) else {
            return (MirrorLookup::Miss, Some(RecordMirrorLookupOutcome::Miss));
        };
        if entry.slot < min_context_slot && !watermark_is_fresh {
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

    pub fn invalidate(&self, record: &Pubkey) {
        self.inner.lock().entries.pop(record);
    }
}

fn resolve_endpoint(
    config: &RecordSyncConfig,
    endpoints: &Endpoints,
) -> Option<(String, String)> {
    let grpc_remote = endpoints.iter().find_map(|endpoint| match endpoint {
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
        .or_else(|| grpc_remote.map(|(_, api_key)| api_key))?;
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
        self.apply(RecordStreamUpdate::SlotAdvanced(slot));
    }

    pub fn test_clear(&self) {
        self.clear();
    }

    pub fn test_apply(&self, update: RecordStreamUpdate) {
        self.apply(update);
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn discovery_backpressure_is_lossless() {
        let mirror = Arc::new(DelegationRecordMirror::with_capacity(16));
        let mut discoveries = mirror.take_discoveries().unwrap();
        for slot in 0..MIRROR_EVENT_CHANNEL_CAPACITY as u64 {
            mirror
                .consume(RecordStreamUpdate::DelegationObserved {
                    delegated_account: [1; 32],
                    record: [2; 32],
                    slot,
                })
                .await;
        }

        let blocked_mirror = Arc::clone(&mirror);
        let blocked = tokio::spawn(async move {
            blocked_mirror
                .consume(RecordStreamUpdate::DelegationObserved {
                    delegated_account: [1; 32],
                    record: [2; 32],
                    slot: MIRROR_EVENT_CHANNEL_CAPACITY as u64,
                })
                .await;
        });
        tokio::task::yield_now().await;
        assert!(!blocked.is_finished());

        assert_eq!(discoveries.recv().await.unwrap().slot, 0);
        blocked.await.unwrap();
        let mut last = None;
        for _ in 0..MIRROR_EVENT_CHANNEL_CAPACITY {
            last = discoveries.recv().await;
        }
        assert_eq!(last.unwrap().slot, MIRROR_EVENT_CHANNEL_CAPACITY as u64);
    }

    #[test]
    fn hit_requires_entry_slot_or_live_watermark() {
        let mirror = DelegationRecordMirror::with_capacity(16);
        let record = Pubkey::new_unique();
        mirror.insert(record, 50, Some(vec![1]));
        assert!(matches!(mirror.get(&record, 100), MirrorLookup::Miss));
        assert!(matches!(mirror.get(&record, 40), MirrorLookup::Hit { .. }));
        mirror.apply(RecordStreamUpdate::SlotAdvanced(120));
        assert!(matches!(
            mirror.get(&record, 100),
            MirrorLookup::Hit { slot: 50, .. }
        ));
        assert!(matches!(mirror.get(&record, 121), MirrorLookup::Miss));
    }

    #[test]
    fn updates_are_monotonic_and_tombstones_are_not_negative_answers() {
        let mirror = DelegationRecordMirror::with_capacity(16);
        let record = Pubkey::new_unique();
        mirror.insert(record, 50, Some(vec![1]));
        mirror.insert(record, 40, Some(vec![9]));
        let MirrorLookup::Hit { data, slot } = mirror.get(&record, 40) else {
            panic!("expected mirror hit");
        };
        assert_eq!((data, slot), (vec![1], 50));
        mirror.insert(record, 60, None);
        assert!(matches!(
            mirror.get(&record, 40),
            MirrorLookup::Tombstone { slot: 60 }
        ));
    }

    #[test]
    fn conflicting_same_slot_updates_require_rpc_confirmation() {
        let mirror = DelegationRecordMirror::with_capacity(16);
        let record = Pubkey::new_unique();
        mirror.insert(record, 50, Some(vec![1]));
        mirror.insert(record, 50, None);
        mirror.insert(record, 50, Some(vec![2]));

        assert!(matches!(
            mirror.get(&record, 50),
            MirrorLookup::Tombstone { slot: 50 }
        ));
    }

    #[test]
    fn interruption_clears_entries_and_watermark() {
        let mirror = DelegationRecordMirror::with_capacity(16);
        let record = Pubkey::new_unique();
        mirror.insert(record, 50, Some(vec![1]));
        mirror.apply(RecordStreamUpdate::SlotAdvanced(120));
        mirror.apply(RecordStreamUpdate::SyncInterrupted);
        assert!(matches!(mirror.get(&record, 1), MirrorLookup::Miss));
        mirror.insert(record, 130, Some(vec![2]));
        assert!(matches!(mirror.get(&record, 140), MirrorLookup::Miss));
    }

    #[test]
    fn eviction_degrades_to_miss() {
        let mirror = DelegationRecordMirror::with_capacity(2);
        let (a, b, c) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        mirror.insert(a, 10, Some(vec![1]));
        mirror.insert(b, 11, Some(vec![2]));
        mirror.insert(c, 12, Some(vec![3]));
        assert!(matches!(mirror.get(&a, 5), MirrorLookup::Miss));
        assert!(matches!(mirror.get(&b, 5), MirrorLookup::Hit { .. }));
        assert!(matches!(mirror.get(&c, 5), MirrorLookup::Hit { .. }));
    }

    #[test]
    fn endpoint_and_api_key_overrides_are_independent() {
        let endpoints = Endpoints::from(
            [Endpoint::Grpc {
                url: "https://configured.example".into(),
                label: "configured".into(),
                api_key: "configured-key".into(),
            }]
            .as_slice(),
        );
        let mut config = RecordSyncConfig {
            endpoint: Some(Url::parse("https://override.example").unwrap()),
            ..Default::default()
        };
        assert_eq!(
            resolve_endpoint(&config, &endpoints),
            Some(
                ("https://override.example/".into(), "configured-key".into(),)
            )
        );

        config.endpoint = None;
        config.api_key = Some("override-key".into());
        assert_eq!(
            resolve_endpoint(&config, &endpoints),
            Some(("https://configured.example".into(), "override-key".into(),))
        );
    }
}
