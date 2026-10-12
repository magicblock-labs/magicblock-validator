//! Streams confirmed account updates through per-provider WebSocket pools.
//!
//! Pool callers must drain events while awaiting operations to avoid blocking
//! acknowledgement delivery. Acknowledgement confirms a subscription, not an
//! initial HTTP fetch. Requests fail when no socket is ready rather than waiting
//! for a reconnect; lost subscriptions are not automatically restored.

use crate::{
    AccountSubscription,
    rpc::{DecodeError, Error as RpcError},
};
use fastwebsockets::WebSocketError;
use hyper::http;
use serde::{Deserialize, Serialize};
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;
use std::io;
use tokio_rustls::rustls::pki_types::InvalidDnsNameError;
use url::Url;

/// Subscription or WebSocket failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// All configured subscription capacity is occupied.
    #[error("all provider subscription limits are exhausted")]
    Capacity,
    /// Capacity exists, but no ready socket can admit the request.
    #[error("capacity is connecting or unavailable; wait for connection events")]
    Unavailable,
    #[error("pool or event receiver closed")]
    Closed,
    /// The peer closed the socket.
    #[error("peer closed the socket")]
    Disconnected,
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("invalid provider message: {0}")]
    Protocol(&'static str),
    #[error(transparent)]
    Json(#[from] json::Error),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Account(#[from] DecodeError),
    #[error(transparent)]
    Socket(#[from] WebSocketError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Http(#[from] http::Error),
    #[error(transparent)]
    ServerName(#[from] InvalidDnsNameError),
    #[error(transparent)]
    Tls(#[from] tokio_rustls::rustls::Error),
}

/// WebSocket result retaining connection, protocol, and subscription failures.
type Result<T> = std::result::Result<T, Error>;

/// Endpoint and capacity limits for one provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Provider {
    /// `ws` or `wss` endpoint with a host.
    pub url: Url,
    /// Maximum sockets, including connection attempts.
    pub max_connections: usize,
    /// Maximum subscriptions per socket, including pending operations.
    pub subs_per_connection: usize,
}

/// Provider settings for confirmed subscriptions; an empty list has no capacity.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config(pub Vec<Provider>);

/// Identity of one connection attempt; reconnects receive a new generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct ConnectionId {
    /// Provider index in [`Config`]'s tuple field.
    provider: usize,
    /// Stable pool entry reused by replacement attempts.
    index: usize,
    /// Incremented on reconnect to distinguish old and replacement sockets.
    generation: u64,
}

/// Account and connection events, ordered within each connection only.
pub(super) enum Event {
    /// Server acknowledged a requested account subscription.
    Acknowledged(AccountSubscription),
    /// Explicit release of the last reference, before unsubscribe acknowledgement.
    /// Revokes buffered-update coverage; the caller owns local account cleanup.
    Removed(Pubkey),
    /// Confirmed account update in `Uninit` mode, before Engine materialization.
    Update {
        /// Remote subscription and optional program address to materialize ProgramData ELF under.
        sub: AccountSubscription,
        /// Decoded account state, including zero-lamport updates.
        account: AccountBuilder,
    },
    /// Lost subscriptions; earlier queued updates precede this event.
    Dropped {
        /// Lost account subscriptions, excluding cancelled requests.
        pubkeys: Vec<Pubkey>,
    },
}

pub(super) use pool::Pool;

mod pool;
mod session;
mod transport;
