//! Primary node: publishes events and holds leader lock.

use magicblock_core::link::replication::Message;
use tokio::sync::mpsc::Receiver;
use tracing::{error, info, instrument, warn};

use super::{ReplicationContext, LOCK_REFRESH_INTERVAL};
use crate::{
    nats::{Producer, Subjects},
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

    /// Runs the state replication until shutdown.
    #[instrument(skip(self))]
    pub async fn run(mut self) {
        info!("entering primary replication mode");
        let mut lock_tick = tokio::time::interval(LOCK_REFRESH_INTERVAL);
        let mut draining = self.ctx.cancel.is_cancelled();

        loop {
            tokio::select! {
                biased;
                _ = self.ctx.cancel.cancelled(), if !draining => {
                    draining = true;
                    info!("shutdown received, draining replication messages");
                }
                _ = lock_tick.tick() => {
                    loop {
                        match self.producer.refresh().await {
                            Ok(locked) if locked => {
                                break;
                            },
                            Ok(_) => {
                                warn!("primary lock lost, should never happen, trying to renew");
                            }
                            Err(e) => {
                                warn!(%e, "lock refresh failed");
                            }
                        }
                    }
                }
                msg = self.messages.recv() => {
                    let Some(msg) = msg else {
                        if let Err(error) = self.producer.release().await {
                            warn!(%error, "failed to release the lock");
                        }
                        return;
                    };
                    while let Err(error) = self.publish(&msg).await {
                        // publish should not easily fail, if that happens, it means
                        // the message broker has become unrecoverably unreacheable
                        warn!(%error, "failed to publish the message");
                    }
                }
                snapshot = self.snapshots.recv(), if !draining => {
                    if let Some((file, slot)) = snapshot {
                        if let Err(e) = self.ctx.upload_snapshot(file, slot).await {
                            warn!(%e, "snapshot upload failed");
                        }
                    }
                }
            }
        }
    }

    async fn publish(&mut self, msg: &Message) -> Result<()> {
        let payload = match bincode::serialize(msg) {
            Ok(p) => p,
            Err(error) => {
                error!(%error, "serialization failed, should never happen");
                return Ok(());
            }
        };
        let subject = Subjects::from_message(msg);
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
