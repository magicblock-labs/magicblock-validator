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

async fn run_primary_readiness_sequence<
    Guard,
    WaitForIdle,
    WaitForIdleFuture,
    ResetBank,
    EnterPrimary,
    EnterPrimaryFuture,
    EnablePrimary,
>(
    wait_for_idle: WaitForIdle,
    reset_bank: ResetBank,
    enter_primary: EnterPrimary,
    enable_primary: EnablePrimary,
) -> Result<()>
where
    WaitForIdle: FnOnce() -> WaitForIdleFuture,
    WaitForIdleFuture: Future<Output = Guard>,
    ResetBank: FnOnce() -> Result<()>,
    EnterPrimary: FnOnce() -> EnterPrimaryFuture,
    EnterPrimaryFuture: Future<Output = ()>,
    EnablePrimary: FnOnce() -> Result<()>,
{
    let _guard = wait_for_idle().await;
    // Account-bank reset is primary-readiness cleanup and is intentionally
    // skipped during standby/replica startup. Run it only after primary work is
    // idle and before exposing primary mode or enabling the chainlink lifecycle.
    reset_bank()?;
    enter_primary().await;
    enable_primary()?;
    Ok(())
}

async fn run_standby_start_sequence<
    Consumer,
    EnterReplica,
    EnterReplicaFuture,
    DisableChainlink,
    CreateConsumer,
    CreateConsumerFuture,
>(
    enter_replica: EnterReplica,
    disable_chainlink: DisableChainlink,
    create_consumer: CreateConsumer,
) -> Option<Consumer>
where
    EnterReplica: FnOnce() -> EnterReplicaFuture,
    EnterReplicaFuture: Future<Output = ()>,
    DisableChainlink: FnOnce(),
    CreateConsumer: FnOnce() -> CreateConsumerFuture,
    CreateConsumerFuture: Future<Output = Option<Consumer>>,
{
    enter_replica().await;
    disable_chainlink();
    create_consumer().await
}

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
    pub async fn into_primary(
        self,
        producer: Producer,
        messages: Receiver<Message>,
    ) -> Result<Primary> {
        let snapshots = self.create_snapshot_watcher()?;
        run_primary_readiness_sequence(
            || self.scheduler.wait_for_idle(),
            || self.chainlink.reset_accounts_bank().map_err(Into::into),
            || self.enter_primary_mode(),
            || self.chainlink.enable_primary().map_err(Into::into),
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
            || self.chainlink.disable(),
            || self.create_consumer(reset),
        )
        .await
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::sync::oneshot;

    use super::*;

    struct TestGuard;

    fn push(events: &Arc<Mutex<Vec<&'static str>>>, event: &'static str) {
        events.lock().unwrap().push(event);
    }

    #[tokio::test]
    async fn primary_readiness_waits_for_idle_before_reset_and_mode_entry() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (release_idle_tx, release_idle_rx) = oneshot::channel::<()>();

        let sequence = {
            let wait_events = events.clone();
            let reset_events = events.clone();
            let enter_events = events.clone();
            let enable_events = events.clone();
            run_primary_readiness_sequence(
                move || async move {
                    push(&wait_events, "wait_for_idle_started");
                    release_idle_rx.await.unwrap();
                    push(&wait_events, "wait_for_idle_completed");
                    TestGuard
                },
                move || {
                    push(&reset_events, "reset_accounts_bank");
                    Ok(())
                },
                move || async move {
                    push(&enter_events, "enter_primary_mode");
                },
                move || {
                    push(&enable_events, "enable_primary");
                    Ok(())
                },
            )
        };

        tokio::pin!(sequence);
        tokio::select! {
            _ = &mut sequence => panic!("primary readiness must wait for idle"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
        }
        assert_eq!(&*events.lock().unwrap(), &["wait_for_idle_started"]);

        release_idle_tx.send(()).unwrap();
        sequence.await.unwrap();
        assert_eq!(
            &*events.lock().unwrap(),
            &[
                "wait_for_idle_started",
                "wait_for_idle_completed",
                "reset_accounts_bank",
                "enter_primary_mode",
                "enable_primary",
            ]
        );
    }

    #[tokio::test]
    async fn standby_start_enters_replica_before_consumer_creation_can_block() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (consumer_started_tx, consumer_started_rx) =
            oneshot::channel::<()>();
        let (release_consumer_tx, release_consumer_rx) =
            oneshot::channel::<()>();

        let sequence = {
            let events = events.clone();
            run_standby_start_sequence(
                {
                    let events = events.clone();
                    move || async move {
                        push(&events, "enter_replica_mode");
                    }
                },
                {
                    let events = events.clone();
                    move || push(&events, "disable_chainlink")
                },
                {
                    let events = events.clone();
                    move || async move {
                        push(&events, "create_consumer_started");
                        consumer_started_tx.send(()).unwrap();
                        release_consumer_rx.await.unwrap();
                        push(&events, "create_consumer_completed");
                        Some(())
                    }
                },
            )
        };

        tokio::pin!(sequence);
        tokio::select! {
            _ = &mut sequence => panic!("consumer creation should still be blocked"),
            _ = consumer_started_rx => {}
        }
        assert_eq!(
            &*events.lock().unwrap(),
            &[
                "enter_replica_mode",
                "disable_chainlink",
                "create_consumer_started",
            ]
        );

        release_consumer_tx.send(()).unwrap();
        assert_eq!(sequence.await, Some(()));
        assert_eq!(
            &*events.lock().unwrap(),
            &[
                "enter_replica_mode",
                "disable_chainlink",
                "create_consumer_started",
                "create_consumer_completed",
            ]
        );
    }
}
