//! Shared context for primary and replica roles.

use std::sync::Arc;

use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::AccountsBankResetter;
use magicblock_core::{
    link::{
        replication::{Block, Message, SuperBlock},
        transactions::{SchedulerMode, TransactionSchedulerHandle},
    },
    Slot, TransactionIndex,
};
use magicblock_ledger::Ledger;
use solana_pubkey::Pubkey;
use tokio::{
    fs::File,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::{Primary, Replica, CONSUMER_RETRY_DELAY};
use crate::{
    nats::{Broker, Consumer, Producer},
    watcher::SnapshotWatcher,
    Error, Result,
};

/// Shared state for both primary and replica roles.
pub struct ReplicationContext<R>
where
    R: AccountsBankResetter,
{
    /// Producer lock owner identifier.
    pub producer_id: String,
    /// Durable consumer identifier.
    pub consumer_id: String,
    /// NATS broker.
    pub broker: Broker,
    /// Global shutdown signal
    pub cancel: CancellationToken,
    /// Scheduler mode channel.
    pub mode_tx: Sender<SchedulerMode>,
    /// Accounts database.
    pub accountsdb: Arc<AccountsDb>,
    /// Reset-only bridge for account-bank cleanup.
    /// TODO(bmuddha): remove this once accounts management is moved to AccountsDb.
    pub account_bank_resetter: Arc<R>,
    /// Transaction ledger.
    pub ledger: Arc<Ledger>,
    /// Transaction scheduler.
    pub scheduler: TransactionSchedulerHandle,
    /// Current position.
    pub slot: Slot,
    /// Position of the last transaction within slot
    pub index: TransactionIndex,
}

impl<R> ReplicationContext<R>
where
    R: AccountsBankResetter,
{
    /// Creates context from ledger state.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        account_bank_resetter: Arc<R>,
        scheduler: TransactionSchedulerHandle,
        cancel: CancellationToken,
        validator_identity: Pubkey,
    ) -> Result<Self> {
        let node_id = validator_identity.to_string();
        let producer_id = format!("{node_id}-producer");
        let consumer_id = format!("{node_id}-consumer");

        let slot = accountsdb.slot();
        let index = ledger
            .get_highest_transaction_index_for_slot(slot)?
            .unwrap_or_default();

        info!(%node_id, %producer_id, %consumer_id, slot, "context initialized");
        Ok(Self {
            producer_id,
            consumer_id,
            broker,
            cancel,
            mode_tx,
            accountsdb,
            account_bank_resetter,
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

    /// Applies a replicated block boundary through the scheduler.
    pub async fn write_block(&self, block: &Block) -> Result<()> {
        self.scheduler
            .replay_block(block.clone())
            .await
            .map_err(Error::Internal)
    }

    /// Verifies superblock checksum.
    pub async fn verify_checksum(&self, sb: &SuperBlock) -> Result<()> {
        let _guard = self.scheduler.wait_for_idle().await;
        // SAFETY: Scheduler is paused, no concurrent modifications during checksum.
        let checksum = unsafe { self.accountsdb.checksum() };
        if checksum == sb.checksum {
            Ok(())
        } else {
            let msg = format!(
                "accountsdb state mismatch at {}, expected {}, got {checksum}",
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
        let mut producer =
            self.broker.create_producer(&self.producer_id).await?;
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

    /// Creates consumer with retry, respecting shutdown signal.
    /// Returns `None` if shutdown is triggered during creation.
    pub async fn create_consumer(&self, reset: bool) -> Option<Consumer> {
        loop {
            tokio::select! {
                result = self.broker.create_consumer(&self.consumer_id, reset) => {
                    match result {
                        Ok(c) => return Some(c),
                        Err(e) => {
                            tracing::warn!(%e, "consumer creation failed, retrying");
                        }
                    }
                }
                _ = self.cancel.cancelled() => {
                    tracing::info!("shutdown during consumer creation");
                    return None;
                }
            }
            tokio::time::sleep(CONSUMER_RETRY_DELAY).await;
        }
    }

    /// Transitions to primary role with the given producer.
    pub async fn into_primary(
        self,
        producer: Producer,
        messages: Receiver<Message>,
    ) -> Result<Primary<R>> {
        let snapshots = self.create_snapshot_watcher()?;
        let _guard = self.scheduler.wait_for_idle().await;
        self.account_bank_resetter.reset_accounts_bank()?;
        self.enter_primary_mode().await;
        Ok(Primary::new(self, producer, messages, snapshots))
    }

    /// Transitions to replica role.
    /// Returns `None` if shutdown is triggered during consumer creation.
    /// reset parameter controls where in the stream the consumption starts:
    /// true - the last known position that we know
    /// false - the last known position that message broker tracks for us
    pub async fn into_replica(self, reset: bool) -> Result<Option<Replica<R>>> {
        let Some(consumer) = self.create_consumer(reset).await else {
            return Ok(None);
        };
        self.enter_replica_mode().await;
        Ok(Some(Replica::new(self, Box::new(consumer))))
    }
}
