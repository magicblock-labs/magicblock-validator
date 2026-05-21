//! Shared context for primary and standby roles.

use std::{future::Future, pin::Pin, sync::Arc};

use machineid_rs::IdBuilder;
use magicblock_accounts_db::AccountsDb;
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

/// Narrow Chainlink lifecycle surface needed by replication.
pub trait PrimaryChainlink: Send + Sync {
    fn reset_accounts_bank(
        &self,
    ) -> magicblock_accounts_db::AccountsDbResult<()>;

    fn enable_primary<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = magicblock_chainlink::errors::ChainlinkResult<()>,
                > + Send
                + 'a,
        >,
    >;

    fn disable<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = magicblock_chainlink::errors::ChainlinkResult<()>,
                > + Send
                + 'a,
        >,
    >;
}

impl<C> PrimaryChainlink
    for magicblock_chainlink::Chainlink<
        magicblock_chainlink::remote_account_provider::chain_rpc_client::ChainRpcClientImpl,
        magicblock_chainlink::submux::SubMuxClient<
            magicblock_chainlink::remote_account_provider::chain_updates_client::ChainUpdatesClient,
        >,
        AccountsDb,
        C,
    >
where
    C: magicblock_chainlink::cloner::Cloner + Send + Sync + 'static,
{
    fn reset_accounts_bank(&self) -> magicblock_accounts_db::AccountsDbResult<()> {
        self.reset_accounts_bank()
    }

    fn enable_primary<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = magicblock_chainlink::errors::ChainlinkResult<()>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.enable_primary().await.map(|_| ())
        })
    }

    fn disable<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = magicblock_chainlink::errors::ChainlinkResult<()>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.disable())
    }
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
    /// Real RPC-facing Chainlink primary-readiness handle.
    pub chainlink: Arc<dyn PrimaryChainlink>,
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
        chainlink: Arc<dyn PrimaryChainlink>,
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
    run_primary_readiness_sequence_with(
        ctx.chainlink.as_ref(),
        &ctx.scheduler,
        &ctx.mode_tx,
    )
    .await
}

async fn run_primary_readiness_sequence_with(
    chainlink: &dyn PrimaryChainlink,
    scheduler: &TransactionSchedulerHandle,
    mode_tx: &Sender<SchedulerMode>,
) -> Result<()> {
    let _guard = scheduler.wait_for_idle().await;
    chainlink.reset_accounts_bank().map_err(Error::from)?;
    chainlink.enable_primary().await.map_err(Error::from)?;
    mode_tx.send(SchedulerMode::Primary).await.map_err(|e| {
        Error::Internal(format!("failed to enter primary mode: {e}"))
    })
}

/// Runs the standby start sequence.
async fn run_standby_start_sequence(
    ctx: &ReplicationContext,
    reset: bool,
) -> Result<Option<Consumer>> {
    run_standby_start_sequence_with(ctx.chainlink.as_ref(), &ctx.mode_tx)
        .await?;
    Ok(ctx.create_consumer(reset).await)
}

async fn run_standby_start_sequence_with(
    chainlink: &dyn PrimaryChainlink,
    mode_tx: &Sender<SchedulerMode>,
) -> Result<()> {
    mode_tx.send(SchedulerMode::Replica).await.map_err(|e| {
        Error::Internal(format!("failed to enter replica mode: {e}"))
    })?;
    chainlink.disable().await.map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use magicblock_chainlink::errors::{ChainlinkError, ChainlinkResult};
    use magicblock_core::link::{link, transactions::SchedulerMode};
    use tokio::sync::mpsc;

    use super::{
        run_primary_readiness_sequence_with, run_standby_start_sequence_with,
        PrimaryChainlink,
    };

    #[derive(Default)]
    struct RecordingPrimaryChainlink {
        calls: Arc<Mutex<Vec<&'static str>>>,
        enable_primary_result: Mutex<Option<ChainlinkResult<()>>>,
    }

    impl RecordingPrimaryChainlink {
        fn with_enable_primary_result(result: ChainlinkResult<()>) -> Self {
            Self {
                calls: Arc::default(),
                enable_primary_result: Mutex::new(Some(result)),
            }
        }

        fn recorded_calls(&self) -> Vec<&'static str> {
            self.calls.lock().expect("calls lock poisoned").clone()
        }
    }

    impl PrimaryChainlink for RecordingPrimaryChainlink {
        fn reset_accounts_bank(
            &self,
        ) -> magicblock_accounts_db::AccountsDbResult<()> {
            self.calls
                .lock()
                .expect("calls lock poisoned")
                .push("reset_accounts_bank");
            Ok(())
        }

        fn enable_primary<'a>(
            &'a self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = ChainlinkResult<()>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls
                    .lock()
                    .expect("calls lock poisoned")
                    .push("enable_primary");
                self.enable_primary_result
                    .lock()
                    .expect("enable_primary_result lock poisoned")
                    .take()
                    .unwrap_or(Ok(()))
            })
        }

        fn disable<'a>(
            &'a self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = ChainlinkResult<()>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls
                    .lock()
                    .expect("calls lock poisoned")
                    .push("disable");
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn run_primary_readiness_sequence_enables_chainlink_before_primary_mode(
    ) {
        let (dispatch, _validator) = link();
        let (mode_tx, mut mode_rx) = mpsc::channel(1);
        let chainlink = RecordingPrimaryChainlink::default();

        run_primary_readiness_sequence_with(
            &chainlink,
            &dispatch.transaction_scheduler,
            &mode_tx,
        )
        .await
        .expect("primary readiness sequence should succeed");

        assert_eq!(
            chainlink.recorded_calls(),
            vec!["reset_accounts_bank", "enable_primary"]
        );
        assert_eq!(mode_rx.try_recv(), Ok(SchedulerMode::Primary));
    }

    #[tokio::test]
    async fn run_standby_start_sequence_disables_chainlink_after_replica_mode()
    {
        let (mode_tx, mut mode_rx) = mpsc::channel(1);
        let chainlink = RecordingPrimaryChainlink::default();

        run_standby_start_sequence_with(&chainlink, &mode_tx)
            .await
            .expect("standby start sequence should succeed");

        assert_eq!(mode_rx.try_recv(), Ok(SchedulerMode::Replica));
        assert_eq!(chainlink.recorded_calls(), vec!["disable"]);
    }

    #[tokio::test]
    async fn run_primary_readiness_sequence_does_not_publish_primary_when_chainlink_enable_fails(
    ) {
        let (dispatch, _validator) = link();
        let (mode_tx, mut mode_rx) = mpsc::channel(1);
        let chainlink = RecordingPrimaryChainlink::with_enable_primary_result(
            Err(ChainlinkError::MissingPrimaryRuntimeBuildConfig),
        );

        let err = run_primary_readiness_sequence_with(
            &chainlink,
            &dispatch.transaction_scheduler,
            &mode_tx,
        )
        .await
        .expect_err("primary readiness sequence should fail");

        assert!(matches!(err, crate::Error::Chainlink(_)));
        assert_eq!(
            chainlink.recorded_calls(),
            vec!["reset_accounts_bank", "enable_primary"]
        );
        assert!(mode_rx.try_recv().is_err());
    }
}
