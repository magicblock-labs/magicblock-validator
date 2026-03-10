//! NATS JetStream client for event replication.
//!
//! # Components
//!
//! - [`Broker`]: Connection manager with stream/bucket initialization
//! - [`Producer`]: Event publisher with distributed leader lock
//! - [`Consumer`]: Event subscriber for standby replay
//! - [`Snapshot`]: AccountsDb snapshot with positioning metadata

use std::collections::HashMap;
use std::time::Duration;

use async_nats::jetstream::{
    consumer::{pull, AckPolicy, DeliverPolicy, PullConsumer},
    kv::{self, CreateErrorKind, Store, UpdateErrorKind},
    object_store::{self, GetErrorKind, ObjectMetadata},
    stream::{self, Compression},
    Context, ContextBuilder,
};
use async_nats::{ConnectOptions, Event, ServerAddr, Subject};
use bytes::Bytes;
use magicblock_core::Slot;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tracing::{debug, error, info, instrument, warn};
use url::Url;

use crate::{Error, Result};

// =============================================================================
// Configuration
// =============================================================================

mod cfg {
    use std::time::Duration;

    // Resource names
    pub const STREAM: &str = "EVENTS";
    pub const SNAPSHOTS: &str = "SNAPSHOTS";
    pub const PRODUCER_LOCK: &str = "PRODUCER";
    pub const LOCK_KEY: &str = "lock";
    pub const SNAPSHOT_NAME: &str = "accountsdb";

    // Metadata keys
    pub const META_SLOT: &str = "slot";
    pub const META_SEQNO: &str = "seqno";

    // Size limits
    pub const STREAM_BYTES: i64 = 256 * 1024 * 1024 * 1024; // 256 GB
    pub const SNAPSHOT_BYTES: i64 = 512 * 1024 * 1024 * 1024; // 512 GB

    // Timeouts
    pub const TTL_STREAM: Duration = Duration::from_secs(24 * 60 * 60);
    pub const TTL_LOCK: Duration = Duration::from_secs(5);
    pub const ACK_WAIT: Duration = Duration::from_secs(30);
    pub const API_TIMEOUT: Duration = Duration::from_secs(2);
    pub const DUP_WINDOW: Duration = Duration::from_secs(30);

    // Reconnect backoff
    pub const RECONNECT_BASE_MS: u64 = 100;
    pub const RECONNECT_MAX_MS: u64 = 5000;

    // Backpressure
    pub const MAX_ACK_PENDING: i64 = 512;
    pub const MAX_ACK_INFLIGHT: usize = 2048;
}

// =============================================================================
// Subjects
// =============================================================================

/// NATS subjects for event types.
pub struct Subjects;

impl Subjects {
    /// Subject for transaction events.
    pub fn transaction() -> Subject {
        Subject::from_static("event.transaction")
    }

    /// Subject for block boundary events.
    pub fn block() -> Subject {
        Subject::from_static("event.block")
    }

    /// Subject for superblock checkpoint events.
    pub fn superblock() -> Subject {
        Subject::from_static("event.superblock")
    }
}

// =============================================================================
// Broker
// =============================================================================

/// NATS JetStream connection with initialized streams and buckets.
///
/// The broker handles:
/// - Event stream (`EVENTS`) for transaction/block/superblock messages
/// - Object store (`SNAPSHOTS`) for AccountsDb snapshots
/// - KV bucket (`PRODUCER`) for leader election
pub struct Broker(Context);

impl Broker {
    /// Connects to NATS and initializes all JetStream resources.
    ///
    /// Resources are created idempotently - safe to call multiple times.
    pub async fn connect(url: Url) -> Result<Self> {
        let addr = ServerAddr::from_url(url)?;

        let client = ConnectOptions::new()
            .max_reconnects(None)
            .reconnect_delay_callback(|attempts| {
                let ms = (attempts as u64 * cfg::RECONNECT_BASE_MS)
                    .min(cfg::RECONNECT_MAX_MS);
                Duration::from_millis(ms)
            })
            .event_callback(|event| async move {
                match event {
                    Event::Disconnected => warn!("NATS disconnected"),
                    Event::Connected => info!("NATS connected"),
                    Event::ClientError(e) => warn!(%e, "NATS client error"),
                    other => debug!(?other, "NATS event"),
                }
            })
            .connect(addr)
            .await?;

        let js = ContextBuilder::new()
            .timeout(cfg::API_TIMEOUT)
            .max_ack_inflight(cfg::MAX_ACK_INFLIGHT)
            .backpressure_on_inflight(true)
            .build(client);

        let broker = Self(js);
        broker.init_resources().await?;
        Ok(broker)
    }

    /// Initializes streams, object stores, and KV buckets.
    async fn init_resources(&self) -> Result<()> {
        let info = self
            .0
            .create_or_update_stream(stream::Config {
                name: cfg::STREAM.into(),
                max_bytes: cfg::STREAM_BYTES,
                subjects: vec![
                    "event.transaction".into(),
                    "event.block".into(),
                    "event.superblock".into(),
                ],
                max_age: cfg::TTL_STREAM,
                duplicate_window: cfg::DUP_WINDOW,
                description: Some("Magicblock validator events".into()),
                compression: Some(Compression::S2),
                ..Default::default()
            })
            .await?;

        info!(stream = %info.config.name, messages = info.state.messages, "JetStream initialized");

        self.0
            .create_object_store(object_store::Config {
                bucket: cfg::SNAPSHOTS.into(),
                description: Some("AccountsDb snapshots".into()),
                max_bytes: cfg::SNAPSHOT_BYTES,
                ..Default::default()
            })
            .await?;

        self.0
            .create_key_value(kv::Config {
                bucket: cfg::PRODUCER_LOCK.into(),
                description: "Producer leader election".into(),
                max_age: cfg::TTL_LOCK,
                ..Default::default()
            })
            .await?;

        Ok(())
    }

    /// Publishes a serialized message to the stream.
    pub async fn publish(
        &self,
        subject: Subject,
        payload: Bytes,
    ) -> Result<()> {
        self.0.publish(subject, payload).await?;
        Ok(())
    }

    /// Retrieves the latest snapshot, if one exists.
    ///
    /// Returns `None` if no snapshot has been uploaded yet.
    pub async fn get_snapshot(&self) -> Result<Option<Snapshot>> {
        let store = self.0.get_object_store(cfg::SNAPSHOTS).await?;

        let mut object = match store.get(cfg::SNAPSHOT_NAME).await {
            Ok(obj) => obj,
            Err(e) if e.kind() == GetErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        let info = object.info();
        let meta = SnapshotMeta::parse(info)?;

        let mut data = Vec::with_capacity(info.size);
        object.read_to_end(&mut data).await?;

        Ok(Some(Snapshot {
            data,
            slot: meta.slot,
            seqno: meta.seqno,
        }))
    }

    /// Uploads a snapshot in the background.
    ///
    /// The snapshot is tagged with the current stream sequence number,
    /// allowing standbys to resume replay from the correct position.
    #[instrument(skip(self, file))]
    pub async fn put_snapshot(&self, slot: Slot, mut file: File) -> Result<()> {
        let store = self.0.get_object_store(cfg::SNAPSHOTS).await?;
        let mut stream = self.0.get_stream(cfg::STREAM).await?;
        let seqno = stream.info().await?.state.last_sequence;

        let meta = ObjectMetadata {
            name: cfg::SNAPSHOT_NAME.into(),
            metadata: SnapshotMeta { slot, seqno }.into_headers(),
            ..Default::default()
        };

        // Background upload to avoid blocking.
        tokio::spawn(async move {
            if let Err(e) = store.put(meta, &mut file).await {
                error!(%e, "snapshot upload failed");
            }
        });

        Ok(())
    }

    /// Creates a consumer for receiving replicated events.
    pub async fn create_consumer(
        &self,
        id: &str,
        start_seq: Option<u64>,
    ) -> Result<Consumer> {
        Consumer::new(id, &self.0, start_seq).await
    }

    /// Creates a producer for publishing events.
    pub async fn create_producer(&self, id: &str) -> Result<Producer> {
        Producer::new(id, &self.0).await
    }
}

// =============================================================================
// Snapshot
// =============================================================================

/// AccountsDb snapshot with positioning metadata.
#[derive(Debug)]
pub struct Snapshot {
    /// Raw snapshot bytes.
    pub data: Vec<u8>,
    /// Slot at which the snapshot was taken.
    pub slot: Slot,
    /// Stream sequence for replay start position.
    pub seqno: u64,
}

/// Metadata stored with each snapshot object.
struct SnapshotMeta {
    slot: Slot,
    seqno: u64,
}

impl SnapshotMeta {
    /// Parses metadata from object info headers.
    fn parse(info: &object_store::ObjectInfo) -> Result<Self> {
        let slot = info
            .metadata
            .get(cfg::META_SLOT)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| {
                Error::Internal("missing 'slot' in snapshot metadata")
            })?;

        let seqno = info
            .metadata
            .get(cfg::META_SEQNO)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| {
                Error::Internal("missing 'seqno' in snapshot metadata")
            })?;

        Ok(Self { slot, seqno })
    }

    /// Converts to HashMap for ObjectMetadata.
    fn into_headers(self) -> HashMap<String, String> {
        HashMap::from([
            (cfg::META_SLOT.into(), self.slot.to_string()),
            (cfg::META_SEQNO.into(), self.seqno.to_string()),
        ])
    }
}

// =============================================================================
// Consumer
// =============================================================================

/// Pull-based consumer for receiving replicated events.
///
/// Supports resuming from a specific sequence number for catch-up replay
/// after recovering from a snapshot.
pub struct Consumer {
    #[allow(dead_code)]
    inner: PullConsumer,
}

impl Consumer {
    async fn new(
        id: &str,
        js: &Context,
        start_seq: Option<u64>,
    ) -> Result<Self> {
        let stream = js.get_stream(cfg::STREAM).await?;

        let deliver_policy = match start_seq {
            Some(seq) => {
                // Delete and recreate to change start position.
                stream.delete_consumer(id).await.ok();
                DeliverPolicy::ByStartSequence {
                    start_sequence: seq,
                }
            }
            None => DeliverPolicy::All,
        };

        let inner = stream
            .get_or_create_consumer(
                id,
                pull::Config {
                    durable_name: Some(id.into()),
                    ack_policy: AckPolicy::All,
                    ack_wait: cfg::ACK_WAIT,
                    max_ack_pending: cfg::MAX_ACK_PENDING,
                    deliver_policy,
                    ..Default::default()
                },
            )
            .await?;

        Ok(Self { inner })
    }
}

// =============================================================================
// Producer
// =============================================================================

/// Event producer with distributed lock for leader election.
///
/// Only one producer can hold the lock at a time, ensuring exactly one
/// primary publishes events. The lock has a TTL and must be refreshed
/// periodically to maintain leadership.
pub struct Producer {
    /// KV store for the lock.
    lock: Store,
    /// Producer identity (node ID).
    id: Bytes,
    /// Current lock revision for CAS updates.
    revision: u64,
}

impl Producer {
    async fn new(id: &str, js: &Context) -> Result<Self> {
        Ok(Self {
            lock: js.get_key_value(cfg::PRODUCER_LOCK).await?,
            id: id.to_owned().into_bytes().into(),
            revision: 0,
        })
    }

    /// Attempts to acquire the leader lock.
    ///
    /// Returns `true` if this producer became the leader.
    /// Returns `false` if another producer already holds the lock.
    pub async fn acquire(&mut self) -> Result<bool> {
        match self.lock.create(cfg::LOCK_KEY, self.id.clone()).await {
            Ok(rev) => {
                self.revision = rev;
                Ok(true)
            }
            Err(e) if e.kind() == CreateErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Refreshes the leader lock to prevent expiration.
    ///
    /// Returns `false` if we lost the lock (another producer took over).
    /// This typically indicates a network partition or slow refresh.
    pub async fn refresh(&mut self) -> Result<bool> {
        match self
            .lock
            .update(cfg::LOCK_KEY, self.id.clone(), self.revision)
            .await
        {
            Ok(rev) => {
                self.revision = rev;
                Ok(true)
            }
            Err(e) if e.kind() == UpdateErrorKind::WrongLastRevision => {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }
}
