//! Primary node: publishes events and holds leader lock.

use magicblock_core::link::replication::Message;
use tokio::sync::mpsc::Receiver;
use tracing::{error, info, instrument, warn};

use super::{ReplicationContext, LOCK_REFRESH_INTERVAL};
use crate::{
    nats::{Producer, Subjects},
    service::Standby,
    watcher::SnapshotWatcher,
    Result,
};

/// Primary node: publishes events and holds leader lock.
pub struct Primary {
    pub(crate) ctx: ReplicationContext,
    producer: Producer,
    messages: Receiver<Message>,
    snapshots: SnapshotWatcher,
}

impl Primary {
    /// Creates a new primary instance.
    pub fn new(
        ctx: ReplicationContext,
        producer: Producer,
        messages: Receiver<Message>,
        snapshots: SnapshotWatcher,
    ) -> Self {
        Self {
            ctx,
            producer,
            messages,
            snapshots,
        }
    }

    /// Runs until leadership lost or shutdown.
    /// Returns `Some(Standby)` on demotion, `None` on shutdown.
    #[instrument(skip(self))]
    pub async fn run(mut self) -> Result<Option<Standby>> {
        info!("entering primary replication mode");
        let mut lock_tick = tokio::time::interval(LOCK_REFRESH_INTERVAL);

        loop {
            tokio::select! {
                biased;
                _ = lock_tick.tick() => {
                    let held = match self.producer.refresh().await {
                        Ok(h) => h,
                        Err(e) => {
                            warn!(%e, "lock refresh failed");
                            false
                        }
                    };
                    if !held {
                        info!("lost leadership, demoting");
                        return self.ctx.into_standby(self.messages, true).await;
                    }
                }
                Some(msg) = self.messages.recv() => {
                    if let Err(error) = self.publish(msg).await {
                        // publish should not easily fail, if that happens, it means
                        // the message broker has become unrecoverably unreacheable
                        warn!(%error, "failed to publish the message");
                        return self.ctx.into_standby(self.messages, true).await;
                    }
                }
                Some((file, slot)) = self.snapshots.recv() => {
                    if let Err(e) = self.ctx.upload_snapshot(file, slot).await {
                        warn!(%e, "snapshot upload failed");
                    }
                }
                _ = self.ctx.cancel.cancelled() => {
                    if let Err(error) = self.producer.release().await {
                        warn!(%error, "failed to release producer lock");
                    }
                    info!("shutdown received, terminating primary mode");
                    return Ok(None);
                }
            }
        }
    }

    async fn publish(&mut self, msg: Message) -> Result<()> {
        let payload = match bincode::serialize(&msg) {
            Ok(p) => p,
            Err(error) => {
                error!(%error, "serialization failed, should never happen");
                return Ok(());
            }
        };
        let subject = Subjects::from_message(&msg);
        let (slot, index) = msg.slot_and_index();
        let ack = matches!(msg, Message::SuperBlock(_));

        self.ctx
            .broker
            .publish(subject, payload.into(), ack)
            .await?;
        self.ctx.update_position(slot, index);
        Ok(())
    }
}
