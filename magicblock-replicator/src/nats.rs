use std::{collections::HashMap, time::Duration};

use crate::{proto::Slot, Error, Result};
use async_nats::{
    jetstream::{
        consumer::{pull, AckPolicy, DeliverPolicy, PullConsumer},
        kv::{self, CreateErrorKind, Store},
        object_store::{self, ObjectMetadata},
        stream::{self, Compression},
        Context, ContextBuilder,
    },
    ConnectOptions, Event, ServerAddr, Subject,
};
use bytes::Bytes;
use tokio::{fs::File, io::AsyncReadExt};
use tracing::{debug, info, warn};
use url::Url;

pub struct AccountsDbSnapshotObject {
    pub blob: Vec<u8>,
    pub slot: Slot,
    pub seqno: u64,
}

struct Consumer {
    inner: PullConsumer,
    jetstream: Context,
    msgcount: usize,
    id: String,
}

struct Producer {
    jetstream: Context,
    store: Store,
    id: Bytes,
}

impl Consumer {
    pub async fn new(
        id: String,
        jetstream: Context,
        seqno: Option<u64>,
    ) -> Result<Self> {
        let stream = jetstream.get_stream("EVENTS").await?;

        let deliver_policy = if let Some(seqno) = seqno {
            stream.delete_consumer(&id).await?;
            DeliverPolicy::ByStartSequence {
                start_sequence: seqno,
            }
        } else {
            DeliverPolicy::All
        };
        let config = pull::Config {
            durable_name: Some(id.clone()),
            ack_policy: AckPolicy::All,
            ack_wait: Duration::from_secs(30),
            max_ack_pending: 512,
            deliver_policy,
            ..Default::default()
        };
        let inner = stream.get_or_create_consumer(&id, config).await?;
        Ok(Self {
            inner,
            jetstream,
            msgcount: 0,
            id,
        })
    }
}

impl Producer {
    pub async fn new(id: String, url: Url) -> Result<Self> {
        let id = id.into_bytes().into();
        let jetstream = setup_jetstream_client(url).await?;
        let store = jetstream.get_key_value("PRODUCER").await?;
        Ok(Self {
            jetstream,
            store,
            id,
        })
    }

    pub async fn publish(
        &mut self,
        payload: Bytes,
        subject: Subject,
    ) -> Result<()> {
        self.jetstream.publish(subject, payload).await?;
        Ok(())
    }

    pub async fn upload_snapshot(
        &mut self,
        slot: Slot,
        mut snapshot: File,
    ) -> Result<()> {
        let store = self.jetstream.get_object_store("SNAPSHOTS").await?;
        let mut stream = self.jetstream.get_stream("EVENTS").await?;
        let seqno = stream.info().await?.state.last_sequence;
        let metadata = {
            let mut map = HashMap::with_capacity(2);
            map.insert("slot".into(), slot.to_string());
            map.insert("seqno".into(), seqno.to_string());
            map
        };
        let meta = ObjectMetadata {
            name: "".into(),
            metadata,
            ..Default::default()
        };
        store.put(meta, &mut snapshot).await?;
        Ok(())
    }

    pub async fn acquire(&self) -> Result<bool> {
        let result = self.store.create("lock", self.id.clone()).await;
        match result {
            Ok(_) => Ok(true),
            Err(e) if matches!(e.kind(), CreateErrorKind::AlreadyExists) => {
                Ok(false)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn update(&self) -> Result<()> {
        self.store.put("lock", self.id.clone()).await?;
        Ok(())
    }
}

pub async fn retrieve_snapshot(
    jetstream: &Context,
) -> Result<AccountsDbSnapshotObject> {
    let store = jetstream.get_object_store("SNAPSHOTS").await?;
    let mut object = store.get("accountsdb").await?;
    let info = object.info();
    let slot = info
        .metadata
        .get("slot")
        .and_then(|s| s.parse::<Slot>().ok())
        .ok_or(Error::Internal(
            "malformed snapshot object, no slot metadata found",
        ))?;
    let seqno = info
        .metadata
        .get("seqno")
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(Error::Internal(
            "malformed snapshot object, no seqno metadata found",
        ))?;

    let mut blob = Vec::with_capacity(info.size);
    object.read_to_end(&mut blob).await?;
    Ok(AccountsDbSnapshotObject { blob, slot, seqno })
}

pub async fn setup_jetstream_client(url: Url) -> Result<Context> {
    let addr = ServerAddr::from_url(url)?;
    // Configure connection options
    let client = ConnectOptions::new()
        .max_reconnects(None) // Infinite reconnect attempts
        .reconnect_delay_callback(|attempts| {
            // Exponential backoff for reconnects
            Duration::from_millis((attempts * 100).min(5000) as u64)
        })
        .event_callback(|event| async move {
            match event {
                Event::Disconnected => warn!("⚠️ NATS Disconnected!"),
                Event::Connected => info!("✅ NATS Connected!"),
                Event::ClientError(err) => {
                    warn!("❌ NATS Client Error: {}", err)
                }
                _ => debug!("ℹ️ NATS Event: {:?}", event),
            }
        })
        .connect(addr)
        .await?;
    let jetstream = ContextBuilder::new()
        .timeout(Duration::from_secs(2))
        .max_ack_inflight(2048)
        .backpressure_on_inflight(true)
        .build(client);

    initialize_jetstream(&jetstream).await?;

    Ok(jetstream)
}

async fn initialize_jetstream(jetstream: &Context) -> Result<()> {
    let config = stream::Config {
        name: "EVENTS".into(),
        max_bytes: 1024 * 1024 * 1024 * 256,
        max_messages: 16 * 1024 * 60 * 60 * 24,
        max_messages_per_subject: -1,
        subjects: vec![
            "event.transaction".into(),
            "event.block".into(),
            "event.superblock".into(),
        ],
        max_consumers: -1,
        max_age: Duration::from_secs(60 * 60 * 24),
        duplicate_window: Duration::from_secs(30),
        description: Some("Magicblock validator events".into()),
        compression: Some(Compression::S2),
        ..Default::default()
    };
    let info = jetstream.create_or_update_stream(config).await?;
    info!(
        "NATS stream existence is confirmed: {} created at: {}, subjects: {:?}, messages: {}",
        info.config.name, info.created, info.config.subjects, info.state.messages
    );
    let config = object_store::Config {
        bucket: "SNAPSHOTS".into(),
        description: Some("Magicblock accountsdb snapshot".into()),
        max_bytes: 512 * 1024 * 1024 * 1024,
        compression: false,
        ..Default::default()
    };
    jetstream.create_object_store(config).await?;
    let config = kv::Config {
        bucket: "PRODUCER".into(),
        description: "Magicblock event producer state".into(),
        max_age: Duration::from_secs(5),
        ..Default::default()
    };
    jetstream.create_key_value(config).await?;
    Ok(())
}
