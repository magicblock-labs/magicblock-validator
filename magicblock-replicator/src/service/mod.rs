//! Primary-Replica state synchronization via NATS JetStream.
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
//!  │ Primary │       │ Replica │
//!  └────┬────┘       └────┬────┘
//!       │                 │
//!   ┌───┴───┐         ┌───┴───┐
//!   │Publish│         │Consume│
//!   │Upload │         │Apply  │
//!   │Refresh│         │Verify │
//!   └───────┘         └───────┘
//! ```

mod context;
mod primary;
mod replica;

use std::{sync::Arc, thread::JoinHandle, time::Duration};

pub use context::ReplicationContext;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::AccountsBankResetter;
use magicblock_config::config::validator::ReplicationMode;
use magicblock_core::link::{
    replication::Message,
    transactions::{SchedulerMode, TransactionSchedulerHandle},
};
use magicblock_ledger::Ledger;
pub use primary::Primary;
pub use replica::Replica;
use tokio::{
    runtime::Builder,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;

use crate::{nats::Broker, Error, Result};

// =============================================================================
// Constants
// =============================================================================

pub(crate) const LOCK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const LEADER_TIMEOUT: Duration = Duration::from_secs(15);
const CONSUMER_RETRY_DELAY: Duration = Duration::from_secs(1);

// =============================================================================
// Service
// =============================================================================

/// Replication service for the selected replication role.
pub enum Service<R>
where
    R: AccountsBankResetter,
{
    Primary(Primary<R>),
    Replica(Replica<R>),
}

impl<R> Service<R>
where
    R: AccountsBankResetter + 'static,
{
    /// Creates service, attempting primary role first if allowed.
    ///
    /// When `can_promote` is false (ReplicaOnly mode), skips lock acquisition
    /// and goes directly to Replica mode.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        account_bank_resetter: Arc<R>,
        scheduler: TransactionSchedulerHandle,
        messages: Receiver<Message>,
        cancel: CancellationToken,
        reset: bool,
        mode: &ReplicationMode,
    ) -> crate::Result<Option<Self>> {
        let ctx = ReplicationContext::new(
            broker,
            mode_tx,
            accountsdb,
            ledger,
            account_bank_resetter,
            scheduler,
            cancel,
        )
        .await?;

        if let ReplicationMode::Primary(_) = mode {
            // Try to become primary
            match ctx.try_acquire_producer().await? {
                Some(producer) => Ok(Some(Self::Primary(
                    ctx.into_primary(producer, messages).await?,
                ))),
                None => Err(Error::Internal(
                    "Failed to acquire producer lock".into(),
                )),
            }
        } else {
            // Replica mode: skip lock acquisition, go directly to Replica
            let Some(replica) = ctx.into_replica(reset).await? else {
                // Shutdown during consumer creation
                return Ok(None);
            };
            Ok(Some(Self::Replica(replica)))
        }
    }

    /// Runs the configured replication role until it exits.
    pub async fn run(self) {
        match self {
            Service::Primary(p) => p.run().await,
            Service::Replica(s) => s.run().await,
        }
    }

    /// Spawns the service in a dedicated OS thread with a single-threaded runtime.
    ///
    /// Returns a `JoinHandle` that yields startup/runtime errors from the
    /// service thread.
    pub fn spawn(self) -> JoinHandle<Result<()>> {
        std::thread::spawn(move || {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .thread_name("replication-service")
                .build()?;

            runtime.block_on(tokio::task::unconstrained(self.run()));
            Ok(())
        })
    }
}
