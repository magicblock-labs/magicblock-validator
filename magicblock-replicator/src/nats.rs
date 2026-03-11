//! NATS JetStream client for event replication.
//!
//! # Components
//!
//! - [`Broker`]: Connection manager with stream/bucket initialization
//! - [`Producer`]: Event publisher with distributed leader lock
//! - [`Consumer`]: Event subscriber for standby replay
//! - [`Snapshot`]: AccountsDb snapshot with positioning metadata

use std::{collections::HashMap, time::Duration};

use async_nats::{
    jetstream::{
        consumer::{
            pull::{Config as PullConfig, Stream as MessageStream},
            AckPolicy, DeliverPolicy, PullConsumer,
        },
        kv::{self, CreateErrorKind, Store, UpdateErrorKind},
        object_store::{self, GetErrorKind, ObjectMetadata},
        stream::{self, Compression},
        Context, ContextBuilder,
    },
    ConnectOptions, Event, ServerAddr, Subject,
};
use bytes::Bytes;
use magicblock_core::Slot;
use tokio::{fs::File, io::AsyncReadExt};
use tracing::{debug, error, info, instrument, warn};
use url::Url;

use crate::{Error, Result};

// =============================================================================
// Configuration
// =============================================================================

/// Resource names and configuration constants.
mod cfg {
    use std::time::Duration;

    pub const STREAM: &str = "EVENTS";
    pub const SNAPSHOTS: &str = "SNAPSHOTS";
    pub const PRODUCER_LOCK: &str = "PRODUCER";
    pub const LOCK_KEY: &str = "lock";
    pub const SNAPSHOT_NAME: &str = "accountsdb";

    pub const META_SLOT: &str = "slot";
    pub const META_SEQNO: &str = "seqno";

    // Size limits (256 GB stream, 512 GB snapshots)
    pub const STREAM_BYTES: i64 = 256 * 1024 * 1024 * 1024;
    pub const SNAPSHOT_BYTES: i64 = 512 * 1024 * 1024 * 1024;

    // Timeouts
    pub const TTL_STREAM: Duration = Duration::from_secs(24 * 60 * 60);
    pub const TTL_LOCK: Duration = Duration::from_secs(5);
    pub const ACK_WAIT: Duration = Duration::from_secs(30);
    pub const API_TIMEOUT: Duration = Duration::from_secs(2);
    pub const DUP_WINDOW: Duration = Duration::from_secs(30);

    // Reconnect backoff (exponential: 100ms base, 5s max)
    pub const RECONNECT_BASE_MS: u64 = 100;
    pub const RECONNECT_MAX_MS: u64 = 5000;

    // Backpressure
    pub const MAX_ACK_PENDING: i64 = 512;
    pub const MAX_ACK_INFLIGHT: usize = 2048;
    pub const BATCH_SIZE: usize = 512;
}

// =============================================================================
// Subjects
// =============================================================================

/// NATS subjects for event types.
///
/// Provides both string constants for stream configuration and typed subjects
/// for publishing.
pub struct Subjects;

impl Subjects {
    pub const TRANSACTION: &'static str = "event.transaction";
    pub const BLOCK: &'static str = "event.block";
    pub const SUPERBLOCK: &'static str = "event.superblock";

    /// All subjects for stream configuration.
    pub const fn all() -> [&'static str; 3] {
        [Self::TRANSACTION, Self::BLOCK, Self::SUPERBLOCK]
    }

    const fn from(s: &'static str) -> Subject {
        Subject::from_static(s)
    }

    /// Typed subject for transaction events.
    pub fn transaction() -> Subject {
        Self::from(Self::TRANSACTION)
    }

    /// Typed subject for block events.
    pub fn block() -> Subject {
        Self::from(Self::BLOCK)
    }

    /// Typed subject for superblock events.
    pub fn superblock() -> Subject {
        Self::from(Self::SUPERBLOCK)
    }
}

// =============================================================================
// Broker
// =============================================================================

/// NATS JetStream connection with initialized streams and buckets.
pub struct Broker {
    ctx: Context,
    seqno: u64,
}

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

        let ctx = ContextBuilder::new()
            .timeout(cfg::API_TIMEOUT)
            .max_ack_inflight(cfg::MAX_ACK_INFLIGHT)
            .backpressure_on_inflight(true)
            .build(client);

        let broker = Self { ctx, seqno: 0 };
        broker.init_resources().await?;
        Ok(broker)
    }

    /// Initializes streams, object stores, and KV buckets.
    async fn init_resources(&self) -> Result<()> {
        let info = self
            .ctx
            .create_or_update_stream(stream::Config {
                name: cfg::STREAM.into(),
                max_bytes: cfg::STREAM_BYTES,
                subjects: Subjects::all().into_iter().map(Into::into).collect(),
                max_age: cfg::TTL_STREAM,
                duplicate_window: cfg::DUP_WINDOW,
                description: Some("Magicblock validator events".into()),
                compression: Some(Compression::S2),
                ..Default::default()
            })
            .await?;

        info!(stream = %info.config.name, messages = info.state.messages, "JetStream initialized");

        self.ctx
            .create_object_store(object_store::Config {
                bucket: cfg::SNAPSHOTS.into(),
                description: Some("AccountsDb snapshots".into()),
                max_bytes: cfg::SNAPSHOT_BYTES,
                ..Default::default()
            })
            .await?;

        self.ctx
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
    ///
    /// If `ack` is true, waits for server acknowledgment and updates internal seqno.
    pub async fn publish(
        &mut self,
        subject: Subject,
        payload: Bytes,
        ack: bool,
    ) -> Result<()> {
        let f = self.ctx.publish(subject, payload).await?;
        if ack {
            self.seqno = f.await?.sequence;
        }
        Ok(())
    }

    /// Retrieves the latest snapshot, if one exists.
    pub async fn get_snapshot(&self) -> Result<Option<Snapshot>> {
        let store = self.ctx.get_object_store(cfg::SNAPSHOTS).await?;

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
        let store = self.ctx.get_object_store(cfg::SNAPSHOTS).await?;
        // Next seqno (snapshot captures state after last published message)
        let seqno = self.seqno + 1;

        let meta = ObjectMetadata {
            name: cfg::SNAPSHOT_NAME.into(),
            metadata: SnapshotMeta { slot, seqno }.into_headers(),
            ..Default::default()
        };

        // Background upload to avoid blocking
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
        Consumer::new(id, &self.ctx, start_seq).await
    }

    /// Creates a producer for publishing events.
    pub async fn create_producer(&self, id: &str) -> Result<Producer> {
        Producer::new(id, &self.ctx).await
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
    /// Parses required metadata fields from object info.
    fn parse(info: &object_store::ObjectInfo) -> Result<Self> {
        let get_parsed =
            |key: &str| info.metadata.get(key).and_then(|v| v.parse().ok());

        let slot = get_parsed(cfg::META_SLOT).ok_or_else(|| {
            Error::Internal("missing 'slot' in snapshot metadata")
        })?;
        let seqno = get_parsed(cfg::META_SEQNO).ok_or_else(|| {
            Error::Internal("missing 'seqno' in snapshot metadata")
        })?;

        Ok(Self { slot, seqno })
    }

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
                // Delete and recreate to change start position
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
                PullConfig {
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

    /// Returns a stream of messages from the consumer.
    ///
    /// Use this in a `tokio::select!` loop to process messages as they arrive.
    /// Messages are fetched in batches for efficiency.
    pub async fn messages(&self) -> Result<MessageStream> {
        self.inner
            .stream()
            .max_messages_per_batch(cfg::BATCH_SIZE)
            .messages()
            .await
            .map_err(Into::into)
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
    lock: Box<Store>,
    id: Bytes,
    revision: u64,
}

impl Producer {
    async fn new(id: &str, js: &Context) -> Result<Self> {
        Ok(Self {
            lock: Box::new(js.get_key_value(cfg::PRODUCER_LOCK).await?),
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
