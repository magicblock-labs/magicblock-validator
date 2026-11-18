#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![doc = include_str!("../README.md")]

#[expect(
    dead_code,
    reason = "Service assembly consumes this private component in a later stack layer"
)]
mod account;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Service assembly consumes HTTP fetches in a later layer")
)]
mod http;
#[expect(
    dead_code,
    reason = "Service assembly consumes this private component in a later stack layer"
)]
mod metrics;
mod rpc;
#[expect(
    dead_code,
    reason = "Service assembly consumes this private component in a later stack layer"
)]
mod websocket;
use account::AccountSubscription;
pub use http::Error as HttpError;
pub use rpc::{DecodeError, Error as RpcError};
pub use websocket::{
    Config as WebSocketConfig, Error as WebSocketError, Provider as WebSocketProvider,
};

#[cfg(test)]
#[path = "../tests/transport/mod.rs"]
mod transport;
