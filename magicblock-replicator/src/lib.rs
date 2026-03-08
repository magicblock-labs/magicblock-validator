//! State replication protocol for streaming transactions from primary to standby nodes.
//!
//! Messages are length-prefixed (4B LE) + bincode payload.

pub mod connection;
pub mod error;
pub mod nats;
pub mod proto;

#[cfg(test)]
mod tests;

pub use error::{Error, Result};
pub use proto::{Message, PROTOCOL_VERSION};
