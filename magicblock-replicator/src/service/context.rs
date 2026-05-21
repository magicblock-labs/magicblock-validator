//! Shared context for primary and standby roles.

use std::sync::Arc;

use machineid_rs::IdBuilder;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::{AccountsBankReset, ChainlinkPrimaryLifecycle};
use magicblock_core::{
    link::{
        replication::{Block, Message, SuperBlock},
        transactions::{SchedulerMode, TransactionSchedulerHandle},
    },
    Slot, TransactionIndex,
};
use magicblock_ledger::Ledger;
use tokio::{
    fs::File,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::{Primary, Standby, CONSUMER_RETRY_DELAY};
use crate::{
    nats::{Broker, Consumer, LockWatcher, Producer},
    watcher::SnapshotWatcher,
    Error, Result,
};

/// Shared state for both primary and standby roles.
pub struct ReplicationContext {
    /// Node identifier for leader election.
    pub id: String,
    /// NATS broker.
    pub broker: Broker,
    /// Global shutdown signal
    pub cancel: CancellationToken,
    /// Scheduler mode channel.
    pub mode_tx: Sender<SchedulerMode>,
    /// Accounts database.
    pub accountsdb: Arc<AccountsDb>,
    /// Reset-only Chainlink cleanup handle for accounts bank readiness.
    pub accounts_bank_reset: Arc<dyn AccountsBankReset>,
    /// Real Chainlink primary lifecycle handle for promotable contexts.
    pub primary_chainlink: Option<Arc<dyn ChainlinkPrimaryLifecycle>>,
    /// Transaction ledger.
    pub ledger: Arc<Ledger>,
    /// Transaction scheduler.
    pub scheduler: TransactionSchedulerHandle,
    /// Current position.
    pub slot: Slot,
    /// Position of the last transaction within slot
    pub index: TransactionIndex,
    /// Whether this node can promote from standby to primary.
    pub can_promote: bool,
}

impl ReplicationContext {
    /// Creates context from ledger state.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        accounts_bank_reset: Arc<dyn AccountsBankReset>,
        primary_chainlink: Option<Arc<dyn ChainlinkPrimaryLifecycle>>,
        scheduler: TransactionSchedulerHandle,
        cancel: CancellationToken,
        can_promote: bool,
    ) -> Result<Self> {
        let id = IdBuilder::new(machineid_rs::Encryption::SHA256)
            .add_component(machineid_rs::HWIDComponent::SystemID)
            .build("magicblock")
            .map_err(|e| Error::Internal(e.to_string()))?;

        if can_promote && primary_chainlink.is_none() {
            return Err(Error::Internal(
                "promotable replication context requires a primary Chainlink lifecycle handle".to_string(),
            ));
        }

        let slot = accountsdb.slot();
        let index = ledger
            .get_highest_transaction_index_for_slot(slot)?
            .unwrap_or_default();

        info!(%id, slot, can_promote, "context initialized");
        Ok(Self {
            id,
            broker,
            cancel,
            mode_tx,
            accountsdb,
            accounts_bank_reset,
            primary_chainlink,
            ledger,
            scheduler,
            slot,
            index,
            can_promote,
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
        let mut producer = self.broker.create_producer(&self.id).await?;
        producer
            .acquire()
            .await
            .map(|acquired| acquired.then_some(producer))
    }

    /// Switches to replica mode.
    pub async fn enter_replica_mode(&self) -> Result<()> {
        self.mode_tx
            .send(SchedulerMode::Replica)
            .await
            .map_err(|e| {
                Error::Internal(format!("failed to enter replica mode: {e}"))
            })
    }

    /// Switches to primary mode.
    pub async fn enter_primary_mode(&self) -> Result<()> {
        self.mode_tx
            .send(SchedulerMode::Primary)
            .await
            .map_err(|e| {
                Error::Internal(format!("failed to enter primary mode: {e}"))
            })
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
                result = self.broker.create_consumer(&self.id, reset) => {
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
    /// Ordering: wait for scheduler idle, reset bank, enable the chainlink
    /// lifecycle, then switch to Primary mode. Reset is primary-readiness
    /// cleanup and is intentionally skipped during standby/replica startup.
    pub async fn into_primary(
        self,
        producer: Producer,
        messages: Receiver<Message>,
    ) -> Result<Primary> {
        let snapshots = self.create_snapshot_watcher()?;
        run_primary_readiness_sequence(&self).await?;
        Ok(Primary::new(self, producer, messages, snapshots))
    }

    /// Transitions to standby role.
    /// Returns `None` if shutdown is triggered during consumer creation.
    /// reset parameter controls where in the stream the consumption starts:
    /// true - the last known position that we know
    /// false - the last known position that message broker tracks for us
    pub async fn into_standby(
        self,
        messages: Receiver<Message>,
        reset: bool,
    ) -> Result<Option<Standby>> {
        let Some(consumer) = run_standby_start_sequence(&self, reset).await?
        else {
            return Ok(None);
        };
        let Some(watcher) = LockWatcher::new(&self.broker, &self.cancel).await
        else {
            return Ok(None);
        };
        Ok(Some(Standby::new(
            self,
            Box::new(consumer),
            messages,
            watcher,
        )))
    }
}

/// Runs the primary readiness sequence.
async fn run_primary_readiness_sequence(
    ctx: &ReplicationContext,
) -> Result<()> {
    let _guard = ctx.scheduler.wait_for_idle().await;
    ctx.accounts_bank_reset
        .reset_accounts_bank()
        .map_err(Error::from)?;
    let primary_chainlink =
        ctx.primary_chainlink.as_ref().ok_or_else(|| {
            Error::Internal(
                "primary Chainlink lifecycle handle is unavailable".to_string(),
            )
        })?;
    primary_chainlink
        .enable_primary()
        .await
        .map_err(Error::from)?;
    if let Err(scheduler_mode_error) = ctx.enter_primary_mode().await {
        if let Err(rollback_error) = primary_chainlink.disable().await {
            error!(
                %rollback_error,
                "Failed to roll back Chainlink primary enable after scheduler primary mode failed"
            );
        }
        return Err(scheduler_mode_error);
    }
    Ok(())
}

/// Runs the standby start sequence.
async fn run_standby_start_sequence(
    ctx: &ReplicationContext,
    reset: bool,
) -> Result<Option<Consumer>> {
    ctx.enter_replica_mode().await?;
    if let Some(primary_chainlink) = &ctx.primary_chainlink {
        primary_chainlink.disable().await.map_err(Error::from)?;
    }
    Ok(ctx.create_consumer(reset).await)
}
