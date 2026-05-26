//! Primary node: publishes events and holds leader lock.

use std::time::Duration;

use magicblock_core::link::replication::Message;
use tokio::{sync::mpsc::Receiver, time::Instant};
use tracing::{error, info, instrument, warn};

use super::{ReplicationContext, LOCK_REFRESH_INTERVAL};
use crate::{
    nats::{Producer, Subjects},
    watcher::SnapshotWatcher,
    Result,
};

const LOCK_RETRY_WARN_INTERVAL: Duration = Duration::from_secs(5);

/// Primary node: publishes events and holds leader lock.
pub struct Primary {
    pub(crate) ctx: ReplicationContext,
    producer: Producer,
    messages: Receiver<Message>,
    snapshots: SnapshotWatcher,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LockState {
    Held,
    Reacquiring { last_warned_at: Option<Instant> },
}

impl LockState {
    fn can_publish(self) -> bool {
        matches!(self, Self::Held)
    }

    fn on_refresh_lost(&mut self) {
        *self = Self::Reacquiring {
            last_warned_at: None,
        };
    }

    fn on_reacquire_result(&mut self, acquired: bool) {
        if acquired {
            *self = Self::Held;
        }
    }

    fn should_warn(&mut self, now: Instant) -> bool {
        let Self::Reacquiring { last_warned_at } = self else {
            return true;
        };

        let should_warn = last_warned_at
            .map(|last| now.duration_since(last) >= LOCK_RETRY_WARN_INTERVAL)
            .unwrap_or(true);
        if should_warn {
            *last_warned_at = Some(now);
        }
        should_warn
    }
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
        let mut lock_state = LockState::Held;
        let mut pending = None;

        loop {
            tokio::select! {
                biased;
                _ = self.ctx.cancel.cancelled(), if !draining => {
                    draining = true;
                    info!("shutdown received, draining replication messages");
                }
                _ = async {}, if lock_state.can_publish() && pending.is_some() => {
                    if let Some(msg) = pending.as_ref() {
                        while let Err(error) = self.publish(msg).await {
                            warn!(%error, "failed to publish the message");
                        }
                    }
                    pending = None;
                }
                _ = lock_tick.tick() => {
                    match lock_state {
                        LockState::Held => match self.producer.refresh().await {
                            Ok(true) => {}
                            Ok(false) => {
                                warn!("primary lock lost, waiting to reacquire");
                                lock_state.on_refresh_lost();
                            }
                            Err(e) => {
                                warn!(%e, "lock refresh failed, waiting to reacquire");
                                lock_state.on_refresh_lost();
                            }
                        },
                        LockState::Reacquiring { .. } => match self.producer.acquire().await {
                            Ok(true) => {
                                info!("primary lock reacquired");
                                lock_state.on_reacquire_result(true);
                            }
                            Ok(false) => {
                                if lock_state.should_warn(Instant::now()) {
                                    warn!("primary lock still unavailable, retrying");
                                }
                            }
                            Err(e) => {
                                if lock_state.should_warn(Instant::now()) {
                                    warn!(%e, "failed to reacquire primary lock");
                                }
                            }
                        }
                    }
                }
                msg = self.messages.recv(), if pending.is_none() => {
                    let Some(msg) = msg else {
                        if lock_state.can_publish() {
                            if let Err(error) = self.producer.release().await {
                                warn!(%error, "failed to release the lock");
                            }
                        }
                        return;
                    };
                    if lock_state.can_publish() {
                        while let Err(error) = self.publish(&msg).await {
                            // publish should not easily fail, if that happens, it means
                            // the message broker has become unrecoverably unreacheable
                            warn!(%error, "failed to publish the message");
                        }
                    } else {
                        pending = Some(msg);
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
        let msg_id = message_id(slot, index);
        let ack = matches!(msg, Message::SuperBlock(_));

        self.ctx
            .broker
            .publish(subject, payload.into(), Some(msg_id.as_str()), ack)
            .await?;
        self.ctx.update_position(slot, index);
        Ok(())
    }
}

fn message_id(slot: u64, index: u32) -> String {
    format!("{slot}:{index}")
}
