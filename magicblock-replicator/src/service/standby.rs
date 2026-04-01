//! Standby node: consumes events and watches for leader failure.

use std::time::{Duration, Instant};

use async_nats::Message as NatsMessage;
use futures::StreamExt;
use magicblock_core::{
    link::transactions::{ReplayPosition, WithEncoded},
    Slot,
};
use solana_transaction::versioned::VersionedTransaction;
use tokio::sync::mpsc::Receiver;
use tracing::{error, info, warn};

use super::{ReplicationContext, LEADER_TIMEOUT};
use crate::{
    nats::{Consumer, LockWatcher},
    proto::TransactionIndex,
    service::Primary,
    Message, Result,
};

/// Standby node: consumes events and watches for leader failure.
pub struct Standby {
    pub(crate) ctx: ReplicationContext,
    consumer: Box<Consumer>,
    messages: Receiver<Message>,
    watcher: LockWatcher,
    last_activity: Instant,
}

impl Standby {
    /// Creates a new standby instance.
    pub fn new(
        ctx: ReplicationContext,
        consumer: Box<Consumer>,
        messages: Receiver<Message>,
        watcher: LockWatcher,
    ) -> Self {
        Self {
            ctx,
            consumer,
            messages,
            watcher,
            last_activity: Instant::now(),
        }
    }

    /// Runs until leadership acquired, returns primary on promotion.
    pub async fn run(mut self) -> Result<Primary> {
        let mut timeout_check = tokio::time::interval(Duration::from_secs(1));
        let mut stream = self.consumer.messages().await;

        loop {
            tokio::select! {
                result = stream.next() => {
                    let Some(result) = result else {
                        stream = self.consumer.messages().await;
                        continue;
                    };
                    match result {
                        Ok(msg) => {
                            self.handle_message(&msg).await;
                            self.last_activity = Instant::now();
                        }
                        Err(e) => warn!(%e, "message consumption stream error"),
                    }
                }

                _ = self.watcher.wait_for_expiry() => {
                    info!("leader lock expired, attempting takeover");
                    if let Ok(Some(producer)) = self.ctx.try_acquire_producer().await {
                        info!("acquired leadership, promoting");
                        return self.ctx.into_primary(producer, self.messages).await;
                    }
                }

                _ = timeout_check.tick(), if self.last_activity.elapsed() > LEADER_TIMEOUT => {
                    if let Ok(Some(producer)) = self.ctx.try_acquire_producer().await {
                        info!("acquired leadership via timeout, promoting");
                        return self.ctx.into_primary(producer, self.messages).await;
                    }
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

        // Skip duplicates.
        let obsolete = self.ctx.slot == slot && self.ctx.index >= index;
        if self.ctx.slot > slot || obsolete {
            return;
        }

        let result = match message {
            Message::Transaction(tx) => {
                self.replay_tx(tx.slot, tx.index, tx.payload).await
            }
            Message::Block(block) => self.ctx.write_block(&block).await,
            Message::SuperBlock(sb) => {
                self.ctx.verify_checksum(&sb).inspect_err(|error|
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

    async fn replay_tx(
        &self,
        slot: Slot,
        index: TransactionIndex,
        encoded: Vec<u8>,
    ) -> Result<()> {
        let pos = ReplayPosition {
            slot,
            index,
            persist: true,
        };
        let tx: VersionedTransaction = bincode::deserialize(&encoded)?;
        let tx = WithEncoded { txn: tx, encoded };
        self.ctx.scheduler.replay(pos, tx).await?;
        Ok(())
    }
}
