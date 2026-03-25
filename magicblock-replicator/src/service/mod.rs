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

mod context;
mod primary;
mod standby;

use std::{sync::Arc, thread::JoinHandle, time::Duration};

pub use context::ReplicationContext;
use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    replication::Message,
    transactions::{SchedulerMode, TransactionSchedulerHandle},
};
use magicblock_ledger::Ledger;
pub use primary::Primary;
pub use standby::Standby;
use tokio::{
    runtime::Builder,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;

use crate::{nats::Broker, Result};

// =============================================================================
// Constants
// =============================================================================

pub(crate) const LOCK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const LEADER_TIMEOUT: Duration = Duration::from_secs(5);
const CONSUMER_RETRY_DELAY: Duration = Duration::from_secs(1);

// =============================================================================
// Service
// =============================================================================

/// Replication service with automatic role transitions.
pub enum Service {
    Primary(Primary),
    Standby(Standby),
}

impl Service {
    /// Creates service, attempting primary role first if allowed.
    ///
    /// When `can_promote` is false (ReplicaOnly mode), skips lock acquisition
    /// and goes directly to standby mode.
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
        messages: Receiver<Message>,
        cancel: CancellationToken,
        reset: bool,
        can_promote: bool,
    ) -> crate::Result<Option<Self>> {
        let ctx = ReplicationContext::new(
            broker,
            mode_tx,
            accountsdb,
            ledger,
            scheduler,
            cancel,
            can_promote,
        )
        .await?;

        // Try to become primary only if promotion is allowed.
        if can_promote {
            match ctx.try_acquire_producer().await? {
                Some(producer) => Ok(Some(Self::Primary(
                    ctx.into_primary(producer, messages).await?,
                ))),
                None => {
                    let Some(standby) =
                        ctx.into_standby(messages, reset).await?
                    else {
                        // Shutdown during consumer creation
                        return Ok(None);
                    };
                    Ok(Some(Self::Standby(standby)))
                }
            }
        } else {
            // ReplicaOnly mode: skip lock acquisition, go directly to standby
            let Some(standby) = ctx.into_standby(messages, reset).await? else {
                // Shutdown during consumer creation
                return Ok(None);
            };
            Ok(Some(Self::Standby(standby)))
        }
    }

    /// Runs service with automatic role transitions.
    pub async fn run(mut self) -> Result<()> {
        loop {
            self = match self {
                Service::Primary(p) => match p.run().await? {
                    Some(s) => Service::Standby(s),
                    None => return Ok(()),
                },
                Service::Standby(s) => match s.run().await? {
                    Some(p) => Service::Primary(p),
                    None => return Ok(()),
                },
            };
        }
    }

    /// Spawns the service in a dedicated OS thread with a single-threaded runtime.
    ///
    /// Returns a `JoinHandle` that can be used to wait for the service to complete.
    pub fn spawn(self) -> JoinHandle<Result<()>> {
        std::thread::spawn(move || {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .thread_name("replication-service")
                .build()
                .expect("Failed to build replication service runtime");

            runtime.block_on(tokio::task::unconstrained(self.run()))
        })
    }
}
