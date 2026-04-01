//! NATS JetStream client for event replication.
//!
//! # Components
//!
//! - [`Broker`]: Connection manager with stream/bucket initialization
//! - [`Producer`]: Event publisher with distributed leader lock
//! - [`Consumer`]: Event subscriber for standby replay
//! - [`Snapshot`]: AccountsDb snapshot with positioning metadata
//! - [`LockWatcher`]: Watcher for leader lock expiration

mod broker;
mod consumer;
mod lock_watcher;
mod producer;
mod snapshot;

use async_nats::Subject;
pub use broker::Broker;
pub use consumer::{Consumer, MessageStream};
pub use lock_watcher::LockWatcher;
use magicblock_core::link::replication::Message;
pub use producer::Producer;
pub use snapshot::Snapshot;

// =============================================================================
// Configuration
// =============================================================================

/// Resource names and configuration constants.
mod cfg {
    use std::time::Duration;

    pub const STREAM: &str = "EVENTS";
    pub const SNAPSHOTS: &str = "SNAPSHOTS";
    pub const PRODUCER_LOCK: &str = "PRODUCER";
    pub const LOCK_KEY: &str = "lock";
    pub const SNAPSHOT_NAME: &str = "accountsdb";

    pub const META_SLOT: &str = "slot";
    pub const META_SEQUENCE: &str = "sequence";

    // Size limits (256 GB stream, 1 GB snapshots)
    pub const STREAM_BYTES: i64 = 256 * 1024 * 1024 * 1024;
    pub const SNAPSHOT_BYTES: i64 = 1024 * 1024 * 1024;

    // Timeouts
    pub const TTL_STREAM: Duration = Duration::from_secs(24 * 60 * 60);
    pub const TTL_LOCK: Duration = Duration::from_secs(5);
    pub const ACK_WAIT: Duration = Duration::from_secs(30);
    pub const API_TIMEOUT: Duration = Duration::from_secs(2);
    pub const DUP_WINDOW: Duration = Duration::from_secs(30);

    // Reconnect backoff (exponential: 100ms base, 5s max)
    pub const RECONNECT_BASE_MS: u64 = 100;
    pub const RECONNECT_MAX_MS: u64 = 5000;

    // Backpressure
    pub const MAX_ACK_PENDING: i64 = 512;
    pub const MAX_ACK_INFLIGHT: usize = 2048;
    pub const BATCH_SIZE: usize = 512;
}

// =============================================================================
// Subjects
// =============================================================================

/// NATS subjects for event types.
///
/// Provides both string constants for stream configuration and typed subjects
/// for publishing.
pub struct Subjects;

impl Subjects {
    pub const TRANSACTION: &'static str = "event.transaction";
    pub const BLOCK: &'static str = "event.block";
    pub const SUPERBLOCK: &'static str = "event.superblock";

    /// All subjects for stream configuration.
    pub const fn all() -> [&'static str; 3] {
        [Self::TRANSACTION, Self::BLOCK, Self::SUPERBLOCK]
    }

    const fn from(s: &'static str) -> Subject {
        Subject::from_static(s)
    }

    /// Typed subject for transaction events.
    pub fn transaction() -> Subject {
        Self::from(Self::TRANSACTION)
    }

    /// Typed subject for block events.
    pub fn block() -> Subject {
        Self::from(Self::BLOCK)
    }

    /// Typed subject for superblock events.
    pub fn superblock() -> Subject {
        Self::from(Self::SUPERBLOCK)
    }

    pub(crate) fn from_message(msg: &Message) -> Subject {
        match msg {
            Message::Transaction(_) => Subjects::transaction(),
            Message::Block(_) => Subjects::block(),
            Message::SuperBlock(_) => Subjects::superblock(),
        }
    }
}
