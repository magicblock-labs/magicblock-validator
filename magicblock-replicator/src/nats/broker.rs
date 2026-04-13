//! NATS JetStream connection with initialized streams and buckets.

use std::time::Duration;

use async_nats::{
    jetstream::{
        kv,
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

use super::{
    cfg, snapshot::SnapshotMeta, Consumer, Producer, Snapshot, Subjects,
};
use crate::Result;

/// NATS JetStream connection with initialized streams and buckets.
pub struct Broker {
    pub(crate) ctx: Context,
    pub(crate) sequence: u64,
}

impl Broker {
    /// Connects to NATS and initializes all JetStream resources.
    ///
    /// Resources are created idempotently - safe to call multiple times.
    /// secret argument: is the NATS nkey secret, which must have a paired
    /// public key stored in the server
    pub async fn connect(url: Url, secret: String) -> Result<Self> {
        let addr = ServerAddr::from_url(url)?;

        let client = ConnectOptions::new()
            .nkey(secret)
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

        let mut broker = Self { ctx, sequence: 0 };
        broker.init_resources().await?;
        Ok(broker)
    }

    /// Initializes streams, object stores, and KV buckets.
    async fn init_resources(&mut self) -> Result<()> {
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

        self.sequence = info.state.first_sequence;

        Ok(())
    }

    /// Publishes a serialized message to the stream.
    ///
    /// If `ack` is true, waits for server acknowledgment and updates internal sequence.
    pub async fn publish(
        &mut self,
        subject: Subject,
        payload: Bytes,
        ack: bool,
    ) -> Result<()> {
        let f = self.ctx.publish(subject, payload).await?;
        if ack {
            self.sequence = f.await?.sequence;
        }
        Ok(())
    }

    /// Retrieves the latest snapshot, if one exists.
    pub async fn get_snapshot(&mut self) -> Result<Option<Snapshot>> {
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
        self.sequence = meta.sequence;

        Ok(Some(Snapshot {
            data,
            slot: meta.slot,
        }))
    }

    /// Uploads a snapshot in the background.
    ///
    /// The snapshot is tagged with the current stream sequence number,
    /// allowing standbys to resume replay from the correct position.
    #[instrument(skip(self, file))]
    pub async fn put_snapshot(&self, slot: Slot, mut file: File) -> Result<()> {
        let store = self.ctx.get_object_store(cfg::SNAPSHOTS).await?;
        // Next sequence (snapshot captures state after last published message)
        let sequence = self.sequence + 1;

        let meta = ObjectMetadata {
            name: cfg::SNAPSHOT_NAME.into(),
            metadata: SnapshotMeta { slot, sequence }.into_headers(),
            ..Default::default()
        };

        // Background upload to avoid blocking
        tokio::spawn(async move {
            if let Err(error) = store.put(meta, &mut file).await {
                error!(%error, "snapshot upload failed");
            } else {
                info!(slot, "uploaded accountsdb snapshot");
            }
        });

        Ok(())
    }

    /// Creates a consumer for receiving replicated events.
    pub async fn create_consumer(
        &self,
        id: &str,
        reset: bool,
    ) -> Result<Consumer> {
        Consumer::new(id, self, reset).await
    }

    /// Creates a producer for publishing events.
    pub async fn create_producer(&self, id: &str) -> Result<Producer> {
        Producer::new(id, &self.ctx).await
    }
}
