//! Pull-based consumer for receiving replicated events.

use async_nats::jetstream::{
    consumer::{
        pull::{Config as PullConfig, Stream as MessageStream},
        AckPolicy, DeliverPolicy, PullConsumer,
    },
    Context,
};
use tracing::warn;

use super::cfg;
use crate::Result;

/// Pull-based consumer for receiving replicated events.
///
/// Supports resuming from a specific sequence number for catch-up replay
/// after recovering from a snapshot.
pub struct Consumer {
    inner: PullConsumer,
}

impl Consumer {
    pub(crate) async fn new(
        id: &str,
        js: &Context,
        start_seq: Option<u64>,
    ) -> Result<Self> {
        let stream = js.get_stream(cfg::STREAM).await?;

        let deliver_policy = match start_seq {
            Some(seq) => {
                // Delete and recreate to change start position
                if let Err(error) = stream.delete_consumer(id).await {
                    warn!(%error, "error removing consumer");
                }
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
    pub async fn messages(&self) -> MessageStream {
        loop {
            let result = self
                .inner
                .stream()
                .max_messages_per_batch(cfg::BATCH_SIZE)
                .messages()
                .await;
            match result {
                Ok(s) => break s,
                Err(error) => {
                    warn!(%error, "failed to create message stream")
                }
            }
        }
    }
}
