//! Shared context for primary and standby roles.

use std::sync::Arc;

use machineid_rs::IdBuilder;
use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    link::transactions::{SchedulerMode, TransactionSchedulerHandle},
    Slot,
};
use magicblock_ledger::Ledger;
use tokio::{
    fs::File,
    sync::mpsc::{Receiver, Sender},
};
use tracing::info;

use super::{Primary, Standby, CONSUMER_RETRY_DELAY};
use crate::{
    nats::{Broker, Consumer, LockWatcher, Producer},
    proto::{self, TransactionIndex},
    watcher::SnapshotWatcher,
    Error, Message, Result,
};

/// Shared state for both primary and standby roles.
pub struct ReplicationContext {
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
    pub index: TransactionIndex,
}

impl ReplicationContext {
    /// Creates context from ledger state.
    pub async fn new(
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
    ) -> Result<Self> {
        let id = IdBuilder::new(machineid_rs::Encryption::SHA256)
            .add_component(machineid_rs::HWIDComponent::SystemID)
            .build("magicblock")
            .map_err(|e| Error::Internal(e.to_string()))?;

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
    pub fn update_position(&mut self, slot: Slot, index: TransactionIndex) {
        self.slot = slot;
        self.index = index;
    }

    /// Writes block to ledger.
    pub async fn write_block(&self, block: &proto::Block) -> Result<()> {
        self.ledger
            .write_block(block.slot, block.timestamp, block.hash)?;
        Ok(())
    }

    /// Verifies superblock checksum.
    pub fn verify_checksum(&self, sb: &proto::SuperBlock) -> Result<()> {
        let _lock = self.accountsdb.lock_database();
        // SAFETY: Lock acquired above ensures no concurrent modifications
        // during checksum computation.
        let checksum = unsafe { self.accountsdb.checksum() };
        if checksum == sb.checksum {
            Ok(())
        } else {
            let msg = format!(
                "accountsdb state mismatch at {}, expected {checksum}, got {}",
                sb.slot, sb.checksum
            );
            Err(Error::Internal(msg))
        }
    }

    /// Creates a snapshot watcher for the database directory.
    pub fn create_snapshot_watcher(&self) -> Result<SnapshotWatcher> {
        SnapshotWatcher::new(self.accountsdb.database_directory())
    }

    /// Attempts to acquire producer lock for primary role.
    pub async fn try_acquire_producer(&self) -> Result<Option<Producer>> {
        let mut producer = self.broker.create_producer(&self.id).await?;
        producer
            .acquire()
            .await
            .map(|acquired| acquired.then_some(producer))
    }

    /// Switches to replica mode.
    pub async fn enter_replica_mode(&self) {
        let _ = self.mode_tx.send(SchedulerMode::Replica).await;
    }

    /// Switches to primary mode.
    pub async fn enter_primary_mode(&self) {
        let _ = self.mode_tx.send(SchedulerMode::Primary).await;
    }

    /// Uploads snapshot.
    pub async fn upload_snapshot(&self, file: File, slot: Slot) -> Result<()> {
        self.broker.put_snapshot(slot, file).await
    }

    /// Creates consumer with retry.
    pub async fn create_consumer(&self, start_seq: Option<u64>) -> Consumer {
        loop {
            match self.broker.create_consumer(&self.id, start_seq).await {
                Ok(c) => return c,
                Err(e) => {
                    tracing::warn!(%e, "consumer creation failed, retrying");
                    tokio::time::sleep(CONSUMER_RETRY_DELAY).await;
                }
            }
        }
    }

    /// Transitions to primary role with the given producer.
    pub async fn into_primary(
        self,
        producer: Producer,
        messages: Receiver<Message>,
    ) -> Result<Primary> {
        let snapshots = self.create_snapshot_watcher()?;
        self.enter_primary_mode().await;
        Ok(Primary::new(self, producer, messages, snapshots))
    }

    /// Transitions to standby role.
    pub async fn into_standby(
        self,
        messages: Receiver<Message>,
    ) -> Result<Standby> {
        let consumer = Box::new(self.create_consumer(None).await);
        let watcher = LockWatcher::new(&self.broker).await;
        self.enter_replica_mode().await;
        Ok(Standby::new(self, consumer, messages, watcher))
    }
}
