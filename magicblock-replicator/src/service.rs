//! Primary-standby state synchronization via NATS JetStream.
//!
//! # Architecture
//!
//! ```text
//!         ┌─────────────┐
//!         │   Service   │
//!         └──────┬──────┘
//!       ┌─────────┴─────────┐
//!       ▼                   ▼
//!  ┌─────────┐       ┌─────────┐
//!  │ Primary │ ←────→│ Standby │
//!  └────┬────┘       └────┬────┘
//!       │                 │
//!   ┌───┴───┐         ┌───┴───┐
//!   │Publish│         │Consume│
//!   │Upload │         │Apply  │
//!   │Refresh│         │Verify │
//!   └───────┘         └───────┘
//! ```

use std::{
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

use async_nats::Message as NatsMessage;
use futures::StreamExt;
use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    link::transactions::{
        ReplayPosition, SchedulerMode, TransactionSchedulerHandle, WithEncoded,
    },
    Slot,
};
use magicblock_ledger::Ledger;
use solana_transaction::versioned::VersionedTransaction;
use tokio::{
    fs::File,
    runtime::Builder,
    sync::mpsc::{Receiver, Sender},
    time::interval,
};
use tracing::{error, info, warn};

pub use crate::nats::Snapshot as AccountsDbSnapshot;
use crate::{
    nats::{Broker, Consumer, Producer},
    proto::{Block, SuperBlock, TxIndex},
    watcher::SnapshotWatcher,
    Message, Result,
};

// =============================================================================
// Constants
// =============================================================================

const LOCK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const LEADER_TIMEOUT: Duration = Duration::from_secs(10);
const CONSUMER_RETRY_DELAY: Duration = Duration::from_secs(1);

// =============================================================================
// Context
// =============================================================================

/// Shared state for both roles.
pub struct Context {
    /// Node identifier for leader election.
    pub id: String,
    /// NATS broker.
    pub broker: Broker,
    /// Scheduler mode channel.
    pub mode_tx: Sender<SchedulerMode>,
    /// Accounts database.
    pub accountsdb: Arc<AccountsDb>,
    /// Transaction ledger.
    pub ledger: Arc<Ledger>,
    /// Transaction scheduler.
    pub scheduler: TransactionSchedulerHandle,
    /// Current position.
    pub slot: Slot,
    pub index: TxIndex,
}

impl Context {
    /// Creates context from ledger state.
    pub async fn new(
        id: String,
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
    ) -> Result<Self> {
        let (slot, index) = ledger
            .get_latest_transaction_position()?
            .unwrap_or_default();

        info!(%id, slot, index, "context initialized");
        Ok(Self {
            id,
            broker,
            mode_tx,
            accountsdb,
            ledger,
            scheduler,
            slot,
            index,
        })
    }

    /// Updates position.
    fn advance(&mut self, slot: Slot, index: TxIndex) {
        self.slot = slot;
        self.index = index;
    }

    /// Writes block to ledger.
    async fn write_block(&self, block: &Block) -> Result<()> {
        self.ledger
            .write_block(block.slot, block.timestamp, block.hash)?;
        Ok(())
    }

    /// Verifies superblock checksum.
    fn verify_checksum(&self, sb: &SuperBlock) -> Result<()> {
        let _lock = self.accountsdb.lock_database();
        // SAFETY: Lock acquired above ensures no concurrent modifications
        // during checksum computation.
        let checksum = unsafe { self.accountsdb.checksum() };
        if checksum == sb.checksum {
            Ok(())
        } else {
            Err(crate::Error::Internal("accountsdb state mismatch"))
        }
    }

    /// Creates a snapshot watcher for the database directory.
    fn create_snapshot_watcher(&self) -> Result<SnapshotWatcher> {
        SnapshotWatcher::new(self.accountsdb.database_directory())
    }

    /// Attempts to acquire producer lock for primary role.
    async fn try_acquire_producer(&self) -> Option<Producer> {
        let mut producer = self.broker.create_producer(&self.id).await.ok()?;
        producer.acquire().await.ok()?.then_some(producer)
    }

    /// Switches to replica mode.
    async fn enter_replica_mode(&self) {
        let _ = self.mode_tx.send(SchedulerMode::Replica).await;
    }

    /// Switches to primary mode.
    async fn enter_primary_mode(&self) {
        let _ = self.mode_tx.send(SchedulerMode::Primary).await;
    }

    /// Uploads snapshot.
    async fn upload_snapshot(&self, file: File, slot: Slot) -> Result<()> {
        self.broker.put_snapshot(slot, file).await
    }

    /// Creates consumer with retry.
    async fn create_consumer(
        &self,
        start_seq: Option<u64>,
    ) -> Result<Consumer> {
        loop {
            match self.broker.create_consumer(&self.id, start_seq).await {
                Ok(c) => return Ok(c),
                Err(e) => {
                    warn!(%e, "consumer creation failed, retrying");
                    tokio::time::sleep(CONSUMER_RETRY_DELAY).await;
                }
            }
        }
    }

    /// Transitions to standby.
    async fn into_standby(
        self,
        messages: Receiver<Message>,
    ) -> Result<Standby> {
        let consumer = Box::new(self.create_consumer(None).await?);
        self.enter_replica_mode().await;
        Ok(Standby {
            ctx: self,
            consumer,
            messages,
            last_activity: Instant::now(),
        })
    }
}

// =============================================================================
// Service
// =============================================================================

/// Replication service with automatic role transitions.
pub enum Service {
    Primary(Primary),
    Standby(Standby),
}

impl Service {
    /// Creates service, attempting primary role first.
    pub async fn new(
        id: String,
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
        messages: Receiver<Message>,
    ) -> Result<Self> {
        let ctx =
            Context::new(id, broker, mode_tx, accountsdb, ledger, scheduler)
                .await?;

        // Try to become primary.
        let Some(producer) = ctx.try_acquire_producer().await else {
            return Ok(Self::Standby(ctx.into_standby(messages).await?));
        };

        ctx.enter_primary_mode().await;
        let snapshots = ctx.create_snapshot_watcher()?;
        Ok(Self::Primary(Primary {
            ctx,
            producer,
            messages,
            snapshots,
        }))
    }

    /// Runs service with automatic role transitions.
    pub async fn run(self) {
        let mut state = self;
        loop {
            state = match state {
                Service::Primary(p) => match p.run().await {
                    Some(s) => Service::Standby(s),
                    None => return,
                },
                Service::Standby(s) => match s.run().await {
                    Some(p) => Service::Primary(p),
                    None => return,
                },
            };
        }
    }

    /// Spawns the service in a dedicated OS thread with a single-threaded runtime.
    ///
    /// Returns a `JoinHandle` that can be used to wait for the service to complete.
    pub fn spawn(self) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let runtime = Builder::new_current_thread()
                .thread_name("replication-service")
                .build()
                .expect("Failed to build replication service runtime");

            runtime.block_on(tokio::task::unconstrained(self.run()));
        })
    }
}

// =============================================================================
// Primary
// =============================================================================

/// Primary node: publishes events and holds leader lock.
pub struct Primary {
    ctx: Context,
    producer: Producer,
    messages: Receiver<Message>,
    snapshots: SnapshotWatcher,
}

impl Primary {
    /// Runs until leadership lost, returns standby on demotion.
    async fn run(mut self) -> Option<Standby> {
        let mut lock_tick = interval(LOCK_REFRESH_INTERVAL);

        loop {
            tokio::select! {
                Some(msg) = self.messages.recv() => {
                    self.publish(msg).await;
                }

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
                        return self.ctx.into_standby(self.messages).await
                            .inspect_err(|e| error!(%e, "demotion failed"))
                            .ok();
                    }
                }

                Some((file, slot)) = self.snapshots.recv() => {
                    if let Err(e) = self.ctx.upload_snapshot(file, slot).await {
                        warn!(%e, "snapshot upload failed");
                    }
                }
            }
        }
    }

    async fn publish(&mut self, msg: Message) {
        let payload = match bincode::serialize(&msg) {
            Ok(p) => p,
            Err(e) => {
                warn!(%e, "serialization failed");
                return;
            }
        };
        let subject = msg.subject();
        let (slot, index) = msg.slot_and_index();
        let ack = matches!(msg, Message::SuperBlock(_));

        if let Err(e) =
            self.ctx.broker.publish(subject, payload.into(), ack).await
        {
            warn!(%e, slot, index, "publish failed");
        } else {
            self.ctx.advance(slot, index);
        }
    }
}

// =============================================================================
// Standby
// =============================================================================

/// Standby node: consumes events and watches for leader failure.
pub struct Standby {
    ctx: Context,
    consumer: Box<Consumer>,
    messages: Receiver<Message>,
    last_activity: Instant,
}

impl Standby {
    /// Runs until leadership acquired, returns primary on promotion.
    async fn run(mut self) -> Option<Primary> {
        let mut timeout_check = interval(Duration::from_secs(1));
        let Ok(mut stream) = self.consumer.messages().await else {
            error!("failed to get message stream");
            return None;
        };

        loop {
            tokio::select! {
                Some(result) = stream.next() => {
                    match result {
                        Ok(msg) => {
                            self.process(&msg).await;
                            self.last_activity = Instant::now();
                        }
                        Err(e) => warn!(%e, "stream error"),
                    }
                }

                _ = timeout_check.tick(), if self.last_activity.elapsed() > LEADER_TIMEOUT => {
                    if let Some(producer) = self.try_acquire_lock().await {
                        info!("acquired leadership, promoting");
                        self.ctx.enter_primary_mode().await;
                        let snapshots = match self.ctx.create_snapshot_watcher() {
                            Ok(s) => s,
                            Err(e) => { error!(%e, "FATAL: snapshot watcher failed"); return None }
                        };
                        return Some(Primary { ctx: self.ctx, producer, messages: self.messages, snapshots });
                    }
                }
            }
        }
    }

    async fn process(&mut self, msg: &NatsMessage) {
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
            warn!(slot, index, %error, "message precessing error");
            return;
        }
        self.ctx.advance(slot, index);
    }

    async fn replay_tx(
        &self,
        slot: Slot,
        index: TxIndex,
        encoded: Vec<u8>,
    ) -> Result<()> {
        let pos = ReplayPosition {
            slot,
            index,
            persist: true,
        };
        let txn: VersionedTransaction = bincode::deserialize(&encoded)?;
        let txn = WithEncoded { txn, encoded };
        self.ctx.scheduler.replay(pos, txn).await?;
        Ok(())
    }

    async fn try_acquire_lock(&mut self) -> Option<Producer> {
        let Ok(mut producer) =
            self.ctx.broker.create_producer(&self.ctx.id).await
        else {
            return None;
        };
        match producer.acquire().await {
            Ok(true) => Some(producer),
            Ok(false) => {
                self.last_activity = Instant::now();
                None
            }
            Err(e) => {
                warn!(%e, "lock acquisition failed");
                None
            }
        }
    }
}
