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
use magicblock_config::config::validator::ReplicationMode;
use magicblock_core::link::{
    replication::Message,
    transactions::{SchedulerMode, TransactionSchedulerHandle},
};
use magicblock_ledger::Ledger;
pub use primary::Primary;
pub use replica::Replica;
use solana_pubkey::Pubkey;
use tokio::{
    runtime::Builder,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::error;
use url::Url;

use crate::{nats::Broker, Result};

// =============================================================================
// Constants
// =============================================================================

pub(crate) const LOCK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const LEADER_TIMEOUT: Duration = Duration::from_secs(15);
const CONSUMER_RETRY_DELAY: Duration = Duration::from_secs(1);

// =============================================================================
// Broker source
// =============================================================================

pub enum BrokerSource {
    Connected(Broker),
    Pending { url: Url, secret: String },
}

// =============================================================================
// Service
// =============================================================================

pub struct Service {
    broker: BrokerSource,
    mode_tx: Sender<SchedulerMode>,
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    scheduler: TransactionSchedulerHandle,
    messages: Receiver<Message>,
    cancel: CancellationToken,
    reset: bool,
    mode: ReplicationMode,
    validator_identity: Pubkey,
}

impl Service {
    /// Creates service, attempting primary role first if allowed.
    ///
    /// When `can_promote` is false (ReplicaOnly mode), skips lock acquisition
    /// and goes directly to Replica mode.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        broker: BrokerSource,
        mode_tx: Sender<SchedulerMode>,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        scheduler: TransactionSchedulerHandle,
        messages: Receiver<Message>,
        cancel: CancellationToken,
        reset: bool,
        mode: ReplicationMode,
        validator_identity: Pubkey,
    ) -> Self {
        Self {
            broker,
            mode_tx,
            accountsdb,
            ledger,
            scheduler,
            messages,
            cancel,
            reset,
            mode,
            validator_identity,
        }
    }

    /// Connects, enters the configured role, and runs until shutdown.
    ///
    /// Failing to connect or acquire the producer lock is fatal: it is logged
    /// and the process exits, since a primary cannot run without them.
    pub async fn run(self) {
        let Self {
            broker,
            mode_tx,
            accountsdb,
            ledger,
            scheduler,
            messages,
            cancel,
            reset,
            mode,
            validator_identity,
        } = self;

        // Connect once (for `Pending`, this runs connect + init_resources here).
        let broker = match broker {
            BrokerSource::Connected(broker) => broker,
            BrokerSource::Pending { url, secret } => {
                match Broker::connect(url, secret).await {
                    Ok(broker) => broker,
                    Err(e) => {
                        error!(%e, "replication broker connect failed");
                        std::process::exit(1);
                    }
                }
            }
        };

        let ctx = match ReplicationContext::new(
            broker,
            mode_tx,
            accountsdb,
            ledger,
            scheduler,
            cancel,
            validator_identity,
        )
        .await
        {
            Ok(ctx) => ctx,
            Err(e) => {
                error!(%e, "failed to initialize replication context");
                std::process::exit(1);
            }
        };

        match mode {
            ReplicationMode::Primary(_) => {
                let producer = match ctx.try_acquire_producer().await {
                    Ok(Some(producer)) => producer,
                    Ok(None) => {
                        error!("failed to acquire producer lock");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        error!(%e, "producer lock acquisition failed");
                        std::process::exit(1);
                    }
                };
                match ctx.into_primary(producer, messages).await {
                    Ok(primary) => primary.run().await,
                    Err(e) => {
                        error!(%e, "failed to enter primary mode");
                        std::process::exit(1);
                    }
                }
            }
            ReplicationMode::Replica { .. } => {
                match ctx.into_replica(reset).await {
                    Ok(Some(replica)) => replica.run().await,
                    // Shutdown was signalled during consumer creation.
                    Ok(None) => {}
                    Err(e) => {
                        error!(%e, "failed to enter replica mode");
                        std::process::exit(1);
                    }
                }
            }
            ReplicationMode::Standalone => {}
        }
    }

    /// Spawns the service on a dedicated OS thread with a single-threaded runtime.
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
