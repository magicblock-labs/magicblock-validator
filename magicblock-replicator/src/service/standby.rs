//! Standby node: consumes events and watches for leader failure.

use std::time::{Duration, Instant};

use async_nats::Message as NatsMessage;
use futures::StreamExt;
use magicblock_core::link::{
    replication::{Message, Transaction},
    transactions::{ReplayPosition, WithEncoded},
};
use solana_transaction::versioned::VersionedTransaction;
use tokio::sync::mpsc::Receiver;
use tracing::{error, info, warn};

use super::{ReplicationContext, LEADER_TIMEOUT};
use crate::{
    nats::{Consumer, LockWatcher},
    service::Primary,
    Result,
};

/// Standby node: consumes events and watches for leader failure.
pub struct Standby {
    pub(crate) ctx: ReplicationContext,
    consumer: Box<Consumer>,
    messages: Receiver<Message>,
    watcher: LockWatcher,
    last_activity: Instant,
    can_promote: bool,
}

impl Standby {
    /// Creates a new standby instance.
    pub fn new(
        ctx: ReplicationContext,
        consumer: Box<Consumer>,
        messages: Receiver<Message>,
        watcher: LockWatcher,
    ) -> Self {
        let can_promote = ctx.can_promote;
        Self {
            ctx,
            consumer,
            messages,
            watcher,
            last_activity: Instant::now(),
            can_promote,
        }
    }

    /// Runs until leadership acquired or shutdown.
    /// Returns `Some(Primary)` on promotion, `None` on shutdown.
    pub async fn run(mut self) -> Result<Option<Primary>> {
        let mut timeout_check = tokio::time::interval(Duration::from_secs(1));
        let Some(mut stream) = self.consumer.messages(&self.ctx.cancel).await
        else {
            return Ok(None);
        };

        loop {
            tokio::select! {
                biased;
                _ = self.watcher.wait_for_expiry() => {
                    if self.has_pending().await {
                        continue;
                    }
                    if !self.can_promote {
                        warn!("leader lock expired, but takeover disabled (ReplicaOnly mode)");
                        continue
                    }
                    info!("leader lock expired, attempting takeover");
                    if let Ok(Some(producer)) = self.ctx.try_acquire_producer().await {
                        info!("acquired leadership, promoting");
                        return self.ctx.into_primary(producer, self.messages).await.map(Some);
                    }
                }
                result = stream.next() => {
                    let Some(result) = result else {
                        if let Some(s) = self.consumer.messages(&self.ctx.cancel).await {
                            stream = s;
                            continue;
                        } else {
                            return Ok(None);
                        };
                    };
                    match result {
                        Ok(msg) => {
                            self.handle_message(&msg).await;
                            self.last_activity = Instant::now();
                        }
                        Err(e) => warn!(%e, "message consumption stream error"),
                    }
                }
                _ = timeout_check.tick(), if self.last_activity.elapsed() > LEADER_TIMEOUT => {
                    if !self.can_promote {
                        warn!("leader timeout reached, but takeover disabled (ReplicaOnly mode)");
                    }
                    if let Ok(Some(producer)) = self.ctx.try_acquire_producer().await {
                        info!("acquired leadership via timeout, promoting");
                        return self.ctx.into_primary(producer, self.messages).await.map(Some);
                    }
                }
                _ = self.ctx.cancel.cancelled() => {
                    info!("shutdown received, terminating standby mode");
                    return Ok(None);
                }
            }
        }
    }

    async fn handle_message(&mut self, msg: &NatsMessage) {
        let message = match bincode::deserialize::<Message>(&msg.payload) {
            Ok(m) => m,
            Err(e) => {
                warn!(%e, "deserialization failed");
                return;
            }
        };
        let (slot, index) = message.slot_and_index();

        let current_slot = self.ctx.slot;
        // Skip duplicates.
        let obsolete = current_slot == slot && self.ctx.index >= index;
        if current_slot > slot || obsolete {
            return;
        }
        if slot.saturating_sub(self.ctx.slot) > 1 {
            error!(slot, current_slot, "slot sequence has been skipped");
        }

        let result = match message {
            Message::Transaction(txn) => {
                self.replay_tx(txn).await
            }
            Message::Block(block) => self.ctx.write_block(&block).await,
            Message::SuperBlock(sb) => {
                self.ctx.verify_checksum(&sb).await.inspect_err(|error|
                    error!(slot, %error, "accountsdb state has diverged")
                )
            }
        };

        if let Err(error) = result {
            warn!(slot, index, %error, "message processing error");
            return;
        }
        self.ctx.update_position(slot, index);
    }

    /// Check whether consumer has any undelivered messages in the stream
    async fn has_pending(&mut self) -> bool {
        self.consumer
            .pending(&self.ctx.cancel)
            .await
            .map(|pending| pending != 0)
            .unwrap_or_default()
    }

    async fn replay_tx(&self, msg: Transaction) -> Result<()> {
        let pos = ReplayPosition {
            slot: msg.slot,
            index: msg.index,
            persist: true,
        };
        let encoded = msg.payload;
        let txn: VersionedTransaction = bincode::deserialize(&encoded)?;
        let txn = WithEncoded { txn, encoded };
        self.ctx.scheduler.replay(pos, txn).await?;
        Ok(())
    }
}
