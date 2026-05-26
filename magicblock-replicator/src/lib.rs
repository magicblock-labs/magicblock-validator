//! State replication protocol for streaming validator events via NATS JetStream.
//!
//! # Architecture
//!
//! The replicator enables primary-replica state replication using NATS JetStream:
//!
//! - **Producer**: Primary node publishes transactions, blocks, and superblocks
//! - **Consumer**: replica nodes consume events to maintain synchronized state
//! - **Snapshots**: Periodic AccountsDb snapshots enable fast replica recovery
//!
//! # Wire Format
//!
//! Messages are serialized with bincode (4-byte discriminator + payload).

pub mod error;
pub mod nats;
pub mod service;
pub mod watcher;

#[cfg(test)]
mod tests;

pub use error::{Error, Result};
pub use service::Service as ReplicationService;
