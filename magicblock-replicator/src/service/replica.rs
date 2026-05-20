//! Replica node: consumes events and watches for leader failure.

use std::time::{Duration, Instant};

use async_nats::jetstream::Message as NatsMessage;
use futures::StreamExt;
use magicblock_core::link::{
    replication::{Message, Transaction},
    transactions::{ReplayPosition, WithEncoded},
};
use solana_transaction::versioned::VersionedTransaction;
use tracing::{error, info, warn};

use super::{ReplicationContext, LEADER_TIMEOUT};
use crate::{nats::Consumer, Result};

/// replica node: consumes events and watches for leader failure.
pub struct Replica {
    pub(crate) ctx: ReplicationContext,
    consumer: Box<Consumer>,
    last_activity: Instant,
}

impl Replica {
    /// Creates a new replica instance.
    pub fn new(ctx: ReplicationContext, consumer: Box<Consumer>) -> Self {
        Self {
            ctx,
            consumer,
            last_activity: Instant::now(),
        }
    }

    /// Runs until leadership acquired or shutdown.
    /// Returns `Some(Primary)` on promotion, `None` on shutdown.
    pub async fn run(mut self) {
        info!("entering replica replication mode");
        let mut timeout_check = tokio::time::interval(Duration::from_secs(1));
        let Some(mut stream) = self.consumer.messages(&self.ctx.cancel).await
        else {
            return;
        };

        loop {
            tokio::select! {
                biased;
                _ = self.ctx.cancel.cancelled() => {
                    info!("shutdown received, terminating replica mode");
                    return;
                }
                result = stream.next() => {
                    let Some(result) = result else {
                        if let Some(s) = self.consumer.messages(&self.ctx.cancel).await {
                            stream = s;
                            continue;
                        } else {
                            return;
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
                _ = timeout_check.tick() => {
                    if self.last_activity.elapsed() < LEADER_TIMEOUT {
                        continue;
                    }
                    self.last_activity = Instant::now();
                    warn!("no leader activity for too long");
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
            Message::Transaction(txn) => self.replay_tx(txn).await,
            Message::Block(block) => {
                let result = self.ctx.write_block(&block).await;
                // NOTE:
                // for performance reasons we batch messages from NATS and ack the
                // entire batch on slot boudaries, instead of on every message
                if result.is_ok() {
                    if let Err(error) = msg.ack().await {
                        warn!(%error, "failed to ack nats message");
                    }
                }
                result
            }
            Message::SuperBlock(sb) => self.ctx.verify_checksum(&sb).await,
        };

        if let Err(error) = result {
            error!(slot, index, %error, "message processing error");
            return;
        }
        self.ctx.update_position(slot, index);
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
