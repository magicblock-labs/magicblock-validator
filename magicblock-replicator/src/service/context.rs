//! Shared context for primary and standby roles.

use std::{future::Future, sync::Arc};

use machineid_rs::IdBuilder;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::StubbedChainlink;
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
use tracing::info;

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
    /// Mocked chainlink to reset accountsdb
    /// TODO(bmuddha): this is a temporary hack, which will be removed
    /// once the accounts management is moved to the accountsdb
    pub chainlink: StubbedChainlink<AccountsDb>,
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
        chainlink: StubbedChainlink<AccountsDb>,
        scheduler: TransactionSchedulerHandle,
        cancel: CancellationToken,
        can_promote: bool,
    ) -> Result<Self> {
        let id = IdBuilder::new(machineid_rs::Encryption::SHA256)
            .add_component(machineid_rs::HWIDComponent::SystemID)
            .build("magicblock")
            .map_err(|e| Error::Internal(e.to_string()))?;

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
            chainlink,
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
        run_primary_readiness_sequence(
            || self.scheduler.wait_for_idle(),
            || self.chainlink.reset_accounts_bank().map_err(Into::into),
            || async {
                self.chainlink.enable_primary().await.map_err(Into::into)
            },
            || self.enter_primary_mode(),
        )
        .await?;
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
        let Some(consumer) = run_standby_start_sequence(
            || self.enter_replica_mode(),
            || async { self.chainlink.disable().await.map_err(Into::into) },
            || self.create_consumer(reset),
        )
        .await?
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
async fn run_primary_readiness_sequence<
    WaitForIdle,
    WaitForIdleFuture,
    IdleGuard,
    ResetBank,
    EnablePrimary,
    EnablePrimaryFuture,
    EnterPrimary,
    EnterPrimaryFuture,
>(
    wait_for_idle: WaitForIdle,
    reset_bank: ResetBank,
    enable_primary: EnablePrimary,
    enter_primary: EnterPrimary,
) -> Result<()>
where
    WaitForIdle: FnOnce() -> WaitForIdleFuture,
    WaitForIdleFuture: Future<Output = IdleGuard>,
    ResetBank: FnOnce() -> Result<()>,
    EnablePrimary: FnOnce() -> EnablePrimaryFuture,
    EnablePrimaryFuture: Future<Output = Result<()>>,
    EnterPrimary: FnOnce() -> EnterPrimaryFuture,
    EnterPrimaryFuture: Future<Output = ()>,
{
    let _guard = wait_for_idle().await;
    reset_bank()?;
    enable_primary().await?;
    enter_primary().await;
    Ok(())
}

/// Runs the standby start sequence.
async fn run_standby_start_sequence<
    Consumer,
    EnterReplica,
    EnterReplicaFuture,
    DisableChainlink,
    DisableChainlinkFuture,
    CreateConsumer,
    CreateConsumerFuture,
>(
    enter_replica: EnterReplica,
    disable_chainlink: DisableChainlink,
    create_consumer: CreateConsumer,
) -> Result<Option<Consumer>>
where
    EnterReplica: FnOnce() -> EnterReplicaFuture,
    EnterReplicaFuture: Future<Output = ()>,
    DisableChainlink: FnOnce() -> DisableChainlinkFuture,
    DisableChainlinkFuture: Future<Output = Result<()>>,
    CreateConsumer: FnOnce() -> CreateConsumerFuture,
    CreateConsumerFuture: Future<Output = Option<Consumer>>,
{
    enter_replica().await;
    disable_chainlink().await?;
    Ok(create_consumer().await)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn record(events: &Arc<Mutex<Vec<&'static str>>>, event: &'static str) {
        events.lock().unwrap().push(event);
    }

    #[tokio::test]
    async fn primary_readiness_sequence_orders_async_chainlink_before_primary_mode(
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));

        run_primary_readiness_sequence(
            {
                let events = Arc::clone(&events);
                move || async move {
                    record(&events, "wait_for_idle_started");
                    record(&events, "wait_for_idle_completed");
                }
            },
            {
                let events = Arc::clone(&events);
                move || {
                    record(&events, "reset_accounts_bank");
                    Ok(())
                }
            },
            {
                let events = Arc::clone(&events);
                move || async move {
                    record(&events, "enable_primary");
                    Ok(())
                }
            },
            {
                let events = Arc::clone(&events);
                move || async move {
                    record(&events, "enter_primary_mode");
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "wait_for_idle_started",
                "wait_for_idle_completed",
                "reset_accounts_bank",
                "enable_primary",
                "enter_primary_mode",
            ]
        );
    }

    #[tokio::test]
    async fn standby_start_sequence_disables_chainlink_before_consumer_creation(
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));

        let consumer = run_standby_start_sequence(
            {
                let events = Arc::clone(&events);
                move || async move {
                    record(&events, "enter_replica_mode");
                }
            },
            {
                let events = Arc::clone(&events);
                move || async move {
                    record(&events, "disable_chainlink");
                    Ok(())
                }
            },
            {
                let events = Arc::clone(&events);
                move || async move {
                    record(&events, "create_consumer_started");
                    Some(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(consumer, Some(()));
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                "enter_replica_mode",
                "disable_chainlink",
                "create_consumer_started",
            ]
        );
    }
}
