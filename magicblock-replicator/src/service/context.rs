//! Shared context for primary and standby roles.

use std::{future::Future, sync::Arc};

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
    run_primary_readiness_sequence_with_hooks(
        || ctx.scheduler.wait_for_idle(),
        ctx.accounts_bank_reset.as_ref(),
        ctx.primary_chainlink.as_deref(),
        || ctx.enter_primary_mode(),
    )
    .await
}

async fn run_primary_readiness_sequence_with_hooks<
    WaitForIdle,
    WaitFut,
    IdleGuard,
    EnterPrimary,
    EnterPrimaryFut,
>(
    wait_for_idle: WaitForIdle,
    accounts_bank_reset: &dyn AccountsBankReset,
    primary_chainlink: Option<&dyn ChainlinkPrimaryLifecycle>,
    enter_primary: EnterPrimary,
) -> Result<()>
where
    WaitForIdle: FnOnce() -> WaitFut,
    WaitFut: Future<Output = IdleGuard>,
    EnterPrimary: FnOnce() -> EnterPrimaryFut,
    EnterPrimaryFut: Future<Output = Result<()>>,
{
    let _guard = wait_for_idle().await;
    accounts_bank_reset
        .reset_accounts_bank()
        .map_err(Error::from)?;
    let primary_chainlink = primary_chainlink.ok_or_else(|| {
        Error::Internal(
            "primary Chainlink lifecycle handle is unavailable".to_string(),
        )
    })?;
    primary_chainlink
        .enable_primary()
        .await
        .map_err(Error::from)?;
    if let Err(scheduler_mode_error) = enter_primary().await {
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
    run_standby_lifecycle_sequence_with_hooks(
        ctx.primary_chainlink.as_deref(),
        || ctx.enter_replica_mode(),
    )
    .await?;
    Ok(ctx.create_consumer(reset).await)
}

async fn run_standby_lifecycle_sequence_with_hooks<
    EnterReplica,
    EnterReplicaFut,
>(
    primary_chainlink: Option<&dyn ChainlinkPrimaryLifecycle>,
    enter_replica: EnterReplica,
) -> Result<()>
where
    EnterReplica: FnOnce() -> EnterReplicaFut,
    EnterReplicaFut: Future<Output = Result<()>>,
{
    enter_replica().await?;
    if let Some(primary_chainlink) = primary_chainlink {
        primary_chainlink.disable().await.map_err(Error::from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use magicblock_accounts_db::AccountsDbResult;
    use magicblock_chainlink::{
        errors::{ChainlinkError, ChainlinkResult},
        ChainlinkPrimaryEnablement,
    };

    use super::*;

    #[derive(Clone)]
    struct RecordingReset {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl AccountsBankReset for RecordingReset {
        fn reset_accounts_bank(&self) -> AccountsDbResult<()> {
            self.events.lock().unwrap().push("reset");
            Ok(())
        }
    }

    struct RecordingPrimaryChainlink {
        events: Arc<Mutex<Vec<&'static str>>>,
        enable_error: bool,
    }

    #[async_trait::async_trait]
    impl ChainlinkPrimaryLifecycle for RecordingPrimaryChainlink {
        async fn enable_primary(
            &self,
        ) -> ChainlinkResult<ChainlinkPrimaryEnablement> {
            self.events.lock().unwrap().push("enable");
            if self.enable_error {
                Err(ChainlinkError::MissingPrimaryRuntimeBuildConfig)
            } else {
                Ok(ChainlinkPrimaryEnablement::Active)
            }
        }

        async fn disable(&self) -> ChainlinkResult<()> {
            self.events.lock().unwrap().push("disable");
            Ok(())
        }
    }

    fn events() -> Arc<Mutex<Vec<&'static str>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn reset(events: Arc<Mutex<Vec<&'static str>>>) -> RecordingReset {
        RecordingReset { events }
    }

    fn chainlink(
        events: Arc<Mutex<Vec<&'static str>>>,
    ) -> RecordingPrimaryChainlink {
        RecordingPrimaryChainlink {
            events,
            enable_error: false,
        }
    }

    fn failing_chainlink(
        events: Arc<Mutex<Vec<&'static str>>>,
    ) -> RecordingPrimaryChainlink {
        RecordingPrimaryChainlink {
            events,
            enable_error: true,
        }
    }

    fn recorded_events(
        events: &Arc<Mutex<Vec<&'static str>>>,
    ) -> Vec<&'static str> {
        events.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn primary_readiness_waits_resets_enables_then_publishes_primary() {
        let events = events();
        let reset = reset(events.clone());
        let chainlink = chainlink(events.clone());

        run_primary_readiness_sequence_with_hooks(
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("idle");
                }
            },
            &reset,
            Some(&chainlink),
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("primary");
                    Ok(())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(
            recorded_events(&events),
            vec!["idle", "reset", "enable", "primary"]
        );
    }

    #[tokio::test]
    async fn primary_readiness_enable_failure_does_not_publish_primary() {
        let events = events();
        let reset = reset(events.clone());
        let chainlink = failing_chainlink(events.clone());

        let result = run_primary_readiness_sequence_with_hooks(
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("idle");
                }
            },
            &reset,
            Some(&chainlink),
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("primary");
                    Ok(())
                }
            },
        )
        .await;

        assert!(matches!(result, Err(Error::Chainlink(_))));
        assert_eq!(recorded_events(&events), vec!["idle", "reset", "enable"]);
    }

    #[tokio::test]
    async fn primary_readiness_missing_lifecycle_handle_does_not_publish_primary(
    ) {
        let events = events();
        let reset = reset(events.clone());

        let result = run_primary_readiness_sequence_with_hooks(
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("idle");
                }
            },
            &reset,
            None,
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("primary");
                    Ok(())
                }
            },
        )
        .await;

        assert!(matches!(result, Err(Error::Internal(_))));
        assert_eq!(recorded_events(&events), vec!["idle", "reset"]);
    }

    #[tokio::test]
    async fn primary_readiness_rolls_back_chainlink_when_primary_publish_fails()
    {
        let events = events();
        let reset = reset(events.clone());
        let chainlink = chainlink(events.clone());

        let result = run_primary_readiness_sequence_with_hooks(
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("idle");
                }
            },
            &reset,
            Some(&chainlink),
            || {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push("primary");
                    Err(Error::Internal("publish failed".to_string()))
                }
            },
        )
        .await;

        assert!(matches!(result, Err(Error::Internal(_))));
        assert_eq!(
            recorded_events(&events),
            vec!["idle", "reset", "enable", "primary", "disable"]
        );
    }

    #[tokio::test]
    async fn standby_lifecycle_enters_replica_before_disabling_chainlink() {
        let events = events();
        let chainlink = chainlink(events.clone());

        run_standby_lifecycle_sequence_with_hooks(Some(&chainlink), || {
            let events = events.clone();
            async move {
                events.lock().unwrap().push("replica");
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(recorded_events(&events), vec!["replica", "disable"]);
    }

    #[tokio::test]
    async fn standby_lifecycle_replica_only_skips_real_chainlink_disable() {
        let events = events();

        run_standby_lifecycle_sequence_with_hooks(None, || {
            let events = events.clone();
            async move {
                events.lock().unwrap().push("replica");
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(recorded_events(&events), vec!["replica"]);
    }
}
