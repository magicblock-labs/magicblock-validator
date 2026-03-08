use std::time::Duration;

use crate::Result;
use async_nats::{
    jetstream::{
        self,
        consumer::{pull, AckPolicy, PullConsumer},
        object_store,
        stream::{self, Compression},
        Context,
    },
    ConnectOptions, Event, ServerAddr,
};
use tracing::{debug, info, warn};
use url::Url;

struct Consumer {
    inner: PullConsumer,
    jetstream: Context,
    id: String,
}

struct Producer {
    jetstream: Context,
    id: String,
}

impl Consumer {
    pub async fn new(id: String, url: Url) -> Result<Self> {
        let jetstream = setup_jetstream_client(url).await?;
        let stream = jetstream.get_stream("EVENTS").await?;
        let config = pull::Config {
            durable_name: Some(id.clone()),
            ack_policy: AckPolicy::All,
            ack_wait: Duration::from_secs(30),
            max_ack_pending: 512,
            ..Default::default()
        };
        let inner = stream.get_or_create_consumer(&id, config).await?;
        Ok(Self {
            inner,
            jetstream,
            id,
        })
    }
}

impl Producer {
    pub async fn new(id: String, url: Url) -> Result<Self> {
        let jetstream = setup_jetstream_client(url).await?;
        Ok(Self { jetstream, id })
    }
}

async fn setup_jetstream_client(url: Url) -> Result<Context> {
    let addr = ServerAddr::from_url(url)?;
    // Configure connection options
    let jetstream = ConnectOptions::new()
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
        .await
        .map(jetstream::new)?;
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
        bucket: "snapshots.accountsdb".into(),
        description: Some("Magicblock accountsdb snapshot".into()),
        max_bytes: 512 * 1024 * 1024 * 1024,
        compression: false,
        ..Default::default()
    };
    jetstream.create_object_store(config).await?;
    Ok(())
}
