//! Streams confirmed updates for tracked accounts and discovers DLP delegation events.
//!
//! Yellowstone reconnects and may replay, but does not guarantee gapless delivery.
//! Delegation matching requires same-slot updates to the account and its derived
//! delegation-record PDA. It assumes at most one delegation per account per slot.
//! Undelegation detection uses static transaction keys, not lookup-table addresses.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;
use url::Url;
use yellowstone_grpc_client::{
    GeyserGrpcBuilderError, GeyserGrpcClientError, SubscribeRequestSinkError,
};
use yellowstone_grpc_proto::{
    cuckoo::{CuckooBuildError, TableFullError},
    tonic,
};

use crate::{AccountSubscription, delegation::Delegation};

/// Configuration, transport, and stream failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("gRPC duplication delay must be nonzero")]
    InvalidDuplicationDelay,
    #[error("gRPC client closed")]
    Closed,
    #[error("gRPC protocol: {0}")]
    Protocol(&'static str),
    #[error(transparent)]
    Build(#[from] GeyserGrpcBuilderError),
    #[error(transparent)]
    Client(#[from] GeyserGrpcClientError),
    #[error(transparent)]
    Status(#[from] tonic::Status),
    #[error(transparent)]
    Send(#[from] SubscribeRequestSinkError),
    #[error(transparent)]
    Filter(#[from] CuckooBuildError),
    #[error(transparent)]
    Capacity(#[from] TableFullError),
}

pub(super) type Result<T> = std::result::Result<T, Error>;

/// Connection settings for one Yellowstone provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct StreamConfig {
    /// HTTP(S) endpoint without URL-embedded credentials.
    pub endpoint: Url,
    /// Optional provider `x-token`.
    #[serde(default)]
    pub token: Option<String>,
    /// Delay before a WebSocket subscription is eligible for gRPC redundancy.
    /// Must be nonzero. Eligible accounts enter the filter on the next periodic
    /// rebuild rather than immediately.
    #[serde(with = "humantime")]
    pub duplication_delay: Duration,
}

/// Ordered account and lifecycle events from one provider.
pub(super) enum Event {
    /// Retained account update in `Uninit` mode, before Engine materialization.
    Update {
        stream: usize,
        /// Remote account address and the local account its updates belong to.
        sub: AccountSubscription,
        /// Raw account update, including zero-lamport updates.
        account: AccountBuilder,
    },
    /// New delegation matched to the account's delegation record PDA.
    Delegated(Delegation),
    /// Canonical DLP request to undelegate a local account, observed at this slot.
    UndelegationRequested { pubkey: Pubkey, slot: u64 },
    /// Accounts undelegated in a successful transaction at this slot.
    Undelegated { pubkeys: SmallVec<[Pubkey; 1]>, slot: u64 },
    /// This account was included in a sent filter for the given subscription generation.
    /// Reports a local send, not server acknowledgement or proof of gapless coverage.
    Confirmed {
        stream: usize,
        pubkey: Pubkey,
        /// Generation assigned by the coverage registry to reject stale confirmations.
        generation: u64,
    },
    /// The outer stream attempt ended; Yellowstone's internal reconnect does not emit this.
    Lost(usize),
}

pub(super) use client::Client;
pub(super) use client::Command;

/// Decodes a wire public key, rejecting any length other than 32 bytes.
fn pubkey(bytes: &[u8]) -> Result<Pubkey> {
    bytes
        .try_into()
        .map(Pubkey::new_from_array)
        .map_err(|_| Error::Protocol("invalid public key"))
}

mod client;
mod matcher;
mod session;
mod transaction;

#[cfg(test)]
mod tests;
