//! Pull-based consumer for receiving replicated events.

pub use async_nats::jetstream::consumer::pull::Stream as MessageStream;
use async_nats::jetstream::consumer::{
    pull::Config as PullConfig, AckPolicy, DeliverPolicy, PullConsumer,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::cfg;
use crate::{nats::Broker, Result};

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
        broker: &Broker,
        reset: bool,
    ) -> Result<Self> {
        let stream = broker.ctx.get_stream(cfg::STREAM).await?;

        let deliver_policy = if reset {
            // Delete and recreate to change start position
            if let Err(error) = stream.delete_consumer(id).await {
                warn!(%error, "error removing consumer");
            }
            DeliverPolicy::ByStartSequence {
                start_sequence: broker.sequence,
            }
        } else {
            DeliverPolicy::All
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
    pub async fn messages(
        &self,
        cancel: &CancellationToken,
    ) -> Option<MessageStream> {
        loop {
            let messages = self
                .inner
                .stream()
                .max_messages_per_batch(cfg::BATCH_SIZE)
                .messages();
            tokio::select! {
                result = messages => {
                    match result {
                        Ok(s) => break Some(s),
                        Err(error) => {
                            warn!(%error, "failed to create message stream")
                        }
                    }
                }
                _ = cancel.cancelled() => {
                    break None;
                }
            }
        }
    }

    pub(crate) async fn pending(
        &self,
        cancel: &CancellationToken,
    ) -> Option<u64> {
        loop {
            tokio::select! {
                result = self.inner.get_info() => {
                    match result {
                        Ok(i) => break Some(i.num_pending),
                        Err(error) => {
                            warn!(%error, "failed to query consumer info")
                        }
                    }
                }
                _ = cancel.cancelled() => {
                    break None;
                }
            }
        }
    }
}
