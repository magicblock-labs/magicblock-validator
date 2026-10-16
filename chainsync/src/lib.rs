#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![doc = include_str!("../README.md")]

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
pub use http::Error as HttpError;
pub use rpc::{DecodeError, Error as RpcError};

#[cfg(test)]
#[path = "../tests/transport/mod.rs"]
mod transport;
