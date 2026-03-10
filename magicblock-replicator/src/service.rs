//! Replication service for primary-standby state synchronization.
//!
//! # Architecture
//!
//! ```text
//!                    ┌─────────────┐
//!                    │   Service   │
//!                    └──────┬──────┘
//!              ┌────────────┴────────────┐
//!              ▼                         ▼
//!        ┌─────────┐               ┌─────────┐
//!        │ Primary │               │ Standby │
//!        └────┬────┘               └────┬────┘
//!             │                         │
//!   ┌─────────┴─────────┐     ┌─────────┴─────────┐
//!   │ Publish events    │     │ Consume events    │
//!   │ Upload snapshots  │     │ Apply to state    │
//!   │ Refresh lock      │     │ Verify checksums  │
//!   └───────────────────┘     └───────────────────┘
//! ```
//!
//! # Role Transitions
//!
//! - **Primary → Standby**: Lock lost (refresh failed)
//! - **Standby → Primary**: Leader lock available (current primary crashed)

use std::sync::Arc;
use std::time::Duration;

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::transactions::{SchedulerMode, TransactionSchedulerHandle};
use magicblock_core::Slot;
use magicblock_ledger::Ledger;
use tokio::sync::mpsc::Receiver;
use tokio::time::interval;
use tracing::{info, warn};

use crate::nats::{Broker, Consumer, Producer, Snapshot};
use crate::proto::{Block, SuperBlock, TxIndex};
use crate::{Message, Result};

/// Re-export for crate users.
pub use crate::nats::Snapshot as AccountsDbSnapshot;

// =============================================================================
// Configuration
// =============================================================================

/// Interval between leader lock refreshes.
const LOCK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum time without seeing leader activity before attempting takeover.
const LEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// Interval between snapshot uploads.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);

/// Retry delay for consumer creation.
const CONSUMER_RETRY_DELAY: Duration = Duration::from_secs(1);

// =============================================================================
// Context
// =============================================================================

/// Shared state accessible from both primary and standby roles.
///
/// Contains all references needed for replication operations:
/// - State stores (accountsdb, ledger)
/// - Communication (broker, scheduler)
/// - Position tracking (slot, index)
pub struct Context {
    /// Unique node identifier used for leader election.
    pub id: String,
    /// NATS broker for publishing/consuming events and snapshots.
    pub broker: Broker,
    /// Channel to switch scheduler between primary/replica mode.
    pub mode_tx: tokio::sync::mpsc::Sender<SchedulerMode>,
    /// Accounts database for snapshot creation and state application.
    pub accountsdb: Arc<AccountsDb>,
    /// Ledger for transaction and block storage.
    pub ledger: Arc<Ledger>,
    /// Transaction scheduler for executing replayed transactions.
    pub scheduler: TransactionSchedulerHandle,
    /// Current slot position in the replicated stream.
    pub slot: Slot,
    /// Current transaction index within the slot.
    pub index: TxIndex,
}

impl Context {
    /// Creates context, initializing position from ledger.
    pub async fn new(
        id: String,
        broker: Broker,
        mode_tx: tokio::sync::mpsc::Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
    ) -> Result<Self> {
        let (slot, index) = ledger
            .get_latest_transaction_position()?
            .unwrap_or_default();

        info!(%id, slot, index, "created replication context");

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

    /// Updates position after processing a message.
    pub fn update_position(&mut self, slot: Slot, index: TxIndex) {
        self.slot = slot;
        self.index = index;
    }

    // =========================================================================
    // Standby Operations
    // =========================================================================

    /// Writes a block to the ledger.
    pub async fn write_block(&self, _block: &Block) -> Result<()> {
        // TODO: Implement ledger block writing.
        // self.ledger.write_block(block)?;
        Ok(())
    }

    /// Verifies superblock checksum against local accountsdb.
    ///
    /// Returns `true` if state matches, `false` if divergence detected.
    pub fn verify_checksum(&self, _superblock: &SuperBlock) -> Result<bool> {
        // TODO: Implement checksum verification.
        // let local_hash = self.accountsdb.compute_hash();
        // Ok(local_hash == superblock.checksum)
        Ok(true)
    }

    /// Applies a snapshot to restore accountsdb state.
    pub async fn apply_snapshot(&self, _snapshot: &Snapshot) -> Result<()> {
        // TODO: Implement snapshot application.
        // self.accountsdb.restore(&snapshot.data)?;
        // Update position to snapshot's seqno for correct replay start.
        Ok(())
    }

    /// Switches scheduler to replica mode for transaction replay.
    pub async fn enter_replica_mode(&self) {
        let _ = self.mode_tx.send(SchedulerMode::Replica).await;
    }

    // =========================================================================
    // Primary Operations
    // =========================================================================

    /// Uploads current accountsdb snapshot to the broker.
    pub async fn upload_snapshot(&self) -> Result<()> {
        // TODO: Get snapshot file from accountsdb and upload.
        // let file = self.accountsdb.snapshot_file().await?;
        // self.broker.put_snapshot(self.slot, file).await
        Ok(())
    }

    /// Switches scheduler to primary mode.
    pub async fn enter_primary_mode(&self) {
        let _ = self.mode_tx.send(SchedulerMode::Primary).await;
    }
}

// =============================================================================
// Service
// =============================================================================

/// Replication service that runs as either primary or standby.
///
/// Automatically selects role based on leader lock availability.
/// Transitions between roles as conditions change.
pub enum Service {
    /// Primary role: publishes events, holds leader lock.
    Primary(Primary),
    /// Standby role: consumes events from stream.
    Standby(Standby),
}

impl Service {
    /// Creates a new replication service.
    ///
    /// Attempts to acquire the leader lock first. If successful, runs as
    /// primary; otherwise, falls back to standby mode.
    pub async fn new(
        id: String,
        broker: Broker,
        mode_tx: tokio::sync::mpsc::Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
        rx: Receiver<Message>,
    ) -> Result<Self> {
        let ctx = Context::new(id, broker, mode_tx, accountsdb, ledger, scheduler).await?;

        // Attempt to acquire leader lock.
        let mut producer = ctx.broker.create_producer(&ctx.id).await?;
        if producer.acquire().await? {
            ctx.enter_primary_mode().await;
            return Ok(Self::Primary(Primary { ctx, producer, rx }));
        }

        // Fall back to standby.
        let standby = ctx.into_standby().await?;
        Ok(Self::Standby(standby))
    }

    /// Runs the service in its assigned role.
    pub async fn run(self) {
        match self {
            Service::Primary(p) => p.run().await,
            Service::Standby(s) => s.run().await,
        }
    }
}

// =============================================================================
// Primary
// =============================================================================

/// Primary node: publishes events and holds the leader lock.
///
/// Responsibilities:
/// - Forward incoming validator events to the stream
/// - Periodically upload accountsdb snapshots
/// - Maintain leadership via lock refresh
pub struct Primary {
    ctx: Context,
    producer: Producer,
    rx: Receiver<Message>,
}

impl Primary {
    /// Main loop: publish events, upload snapshots, maintain lock.
    async fn run(mut self) {
        let mut lock_tick = interval(LOCK_REFRESH_INTERVAL);
        let mut snapshot_tick = interval(SNAPSHOT_INTERVAL);

        loop {
            tokio::select! {
                // Forward incoming messages to the stream.
                Some(msg) = self.rx.recv() => {
                    self.publish(msg).await;
                }

                // Periodically refresh the leader lock.
                _ = lock_tick.tick() => {
                    if !self.refresh_lock().await {
                        info!("lost leadership, demoting to standby");
                        return;
                    }
                }

                // Periodically upload snapshots.
                _ = snapshot_tick.tick() => {
                    self.upload_snapshot().await;
                }
            }
        }
    }

    /// Publishes a message to the stream.
    async fn publish(&mut self, msg: Message) {
        let Ok(payload) = bincode::serialize(&msg) else { return };
        let subject = msg.subject();
        let (slot, index) = msg.slot_and_index();

        if self.ctx.broker.publish(subject, payload.into()).await.is_ok() {
            self.ctx.update_position(slot, index);
        }
    }

    /// Refreshes the leader lock. Returns `false` if lost.
    async fn refresh_lock(&mut self) -> bool {
        match self.producer.refresh().await {
            Ok(held) => held,
            Err(e) => {
                warn!(%e, "failed to refresh leader lock");
                false
            }
        }
    }

    /// Uploads a snapshot of current state.
    async fn upload_snapshot(&self) {
        if let Err(e) = self.ctx.upload_snapshot().await {
            warn!(%e, "failed to upload snapshot");
        }
    }
}

// =============================================================================
// Standby
// =============================================================================

/// Standby node: consumes events and applies them to local state.
///
/// Responsibilities:
/// - Consume events from the stream
/// - Apply transactions via scheduler
/// - Write blocks to ledger
/// - Verify superblock checksums
/// - Watch for leader failure (heartbeat timeout)
/// - Attempt takeover when leader fails
pub struct Standby {
    ctx: Context,
    #[allow(dead_code)]
    consumer: Consumer,
    /// Tracks last time we saw activity from the leader.
    last_leader_activity: tokio::time::Instant,
}

impl Standby {
    /// Main loop: consume events, apply state, watch for leader failure.
    async fn run(mut self) {
        let mut leader_timeout_check = interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                // TODO: Consume messages from the stream.
                // This will be implemented when Consumer has a receive method.
                // For now, we just watch for leader timeout.

                _ = leader_timeout_check.tick() => {
                    if self.last_leader_activity.elapsed() > LEADER_TIMEOUT
                        && self.attempt_takeover().await
                    {
                        return;
                    }
                }
            }
        }
    }

    /// Updates leader activity timestamp (called when receiving events).
    fn record_leader_activity(&mut self) {
        self.last_leader_activity = tokio::time::Instant::now();
    }

    /// Attempts to acquire leadership.
    ///
    /// Returns `true` if successful and we should exit (caller should restart).
    async fn attempt_takeover(&mut self) -> bool {
        info!("leader timeout, attempting takeover");

        let mut producer = match self.ctx.broker.create_producer(&self.ctx.id).await {
            Ok(p) => p,
            Err(e) => {
                warn!(%e, "failed to create producer for takeover");
                return false;
            }
        };

        match producer.acquire().await {
            Ok(true) => {
                info!("successfully acquired leadership");
                self.ctx.enter_primary_mode().await;
                // Exit this run loop; caller should restart as Primary.
                true
            }
            Ok(false) => {
                info!("another node acquired leadership first");
                self.record_leader_activity();
                false
            }
            Err(e) => {
                warn!(%e, "failed to acquire leadership");
                false
            }
        }
    }

    // =========================================================================
    // Event Processing (scaffolding for future implementation)
    // =========================================================================

    /// Processes an incoming transaction message.
    #[allow(dead_code)]
    async fn process_transaction(&mut self, _slot: Slot, _index: TxIndex, _payload: &[u8]) -> Result<()> {
        // TODO:
        // 1. Deserialize transaction
        // 2. Submit to scheduler for execution
        // 3. Update position
        self.record_leader_activity();
        Ok(())
    }

    /// Processes an incoming block message.
    #[allow(dead_code)]
    async fn process_block(&mut self, block: &Block) -> Result<()> {
        // TODO:
        // 1. Write block to ledger
        // 2. Update slot position
        self.ctx.write_block(block).await?;
        self.record_leader_activity();
        Ok(())
    }

    /// Processes an incoming superblock message.
    #[allow(dead_code)]
    async fn process_superblock(&mut self, superblock: &SuperBlock) -> Result<()> {
        // TODO:
        // 1. Verify checksum against local state
        // 2. If mismatch, may need to request snapshot
        if !self.ctx.verify_checksum(superblock)? {
            warn!(
                slot = superblock.slot,
                "superblock checksum mismatch - state divergence detected"
            );
            // TODO: Request snapshot or enter recovery mode
        }
        self.record_leader_activity();
        Ok(())
    }
}

// =============================================================================
// Context -> Role Transitions
// =============================================================================

impl Context {
    /// Transitions to standby role by creating a consumer.
    pub async fn into_standby(self) -> Result<Standby> {
        let consumer = self.create_consumer_with_retry(None).await?;
        self.enter_replica_mode().await;

        Ok(Standby {
            consumer,
            last_leader_activity: tokio::time::Instant::now(),
            ctx: self,
        })
    }

    /// Creates a consumer with retry on failure.
    async fn create_consumer_with_retry(&self, start_seq: Option<u64>) -> Result<Consumer> {
        loop {
            match self.broker.create_consumer(&self.id, start_seq).await {
                Ok(c) => return Ok(c),
                Err(e) => {
                    warn!(%e, "failed to create consumer; retrying");
                    tokio::time::sleep(CONSUMER_RETRY_DELAY).await;
                }
            }
        }
    }

    /// Attempts to recover from a snapshot before starting consumer.
    #[allow(dead_code)]
    pub async fn recover_from_snapshot(&self) -> Result<Option<Snapshot>> {
        let Some(snapshot) = self.broker.get_snapshot().await? else {
            info!("no snapshot available for recovery");
            return Ok(None);
        };

        info!(slot = snapshot.slot, seqno = snapshot.seqno, "retrieved snapshot");
        self.apply_snapshot(&snapshot).await?;
        Ok(Some(snapshot))
    }
}
