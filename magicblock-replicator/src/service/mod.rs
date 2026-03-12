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
use magicblock_core::link::transactions::{
    SchedulerMode, TransactionSchedulerHandle,
};
use magicblock_ledger::Ledger;
pub use primary::Primary;
pub use standby::Standby;
use tokio::{
    runtime::Builder,
    sync::mpsc::{Receiver, Sender},
};

use crate::{nats::Broker, Message, Result};

// =============================================================================
// Constants
// =============================================================================

pub(crate) const LOCK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const LEADER_TIMEOUT: Duration = Duration::from_secs(10);
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
    /// Creates service, attempting primary role first.
    pub async fn new(
        id: String,
        broker: Broker,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
        messages: Receiver<Message>,
    ) -> crate::Result<Self> {
        let ctx = ReplicationContext::new(
            id, broker, mode_tx, accountsdb, ledger, scheduler,
        )
        .await?;

        // Try to become primary.
        match ctx.try_acquire_producer().await? {
            Some(producer) => {
                Ok(Self::Primary(ctx.into_primary(producer, messages).await?))
            }
            None => Ok(Self::Standby(ctx.into_standby(messages).await?)),
        }
    }

    /// Runs service with automatic role transitions.
    pub async fn run(mut self) -> Result<()> {
        loop {
            self = match self {
                Service::Primary(p) => Service::Standby(p.run().await?),
                Service::Standby(s) => match s.run().await {
                    Ok(p) => Service::Primary(p),
                    Err(error) => {
                        tracing::error!(%error, "unrecoverable replication failure");
                        return Err(error);
                    }
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
                .thread_name("replication-service")
                .build()
                .expect("Failed to build replication service runtime");

            runtime.block_on(tokio::task::unconstrained(self.run()))
        })
    }
}
