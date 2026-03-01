//! State replication protocol for streaming transactions from primary to standby nodes.
//!
//! Messages are length-prefixed (4B LE) + bincode payload.

pub mod connection;
pub mod error;
pub mod proto;
pub mod tcp;

pub use connection::{Receiver, Sender};
pub use error::{Error, Result};
pub use proto::{Message, PROTOCOL_VERSION};
