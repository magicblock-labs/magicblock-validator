// NOTE: from rpc-client/src/nonblocking/rpc_client.rs
use solana_sdk::{
    account::{Account, ReadableAccount},
    clock::{Epoch, Slot, UnixTimestamp},
    commitment_config::CommitmentConfig,
    epoch_info::EpochInfo,
    epoch_schedule::EpochSchedule,
    feature::Feature,
    fee_calculator::{FeeCalculator, FeeRateGovernor},
    hash::Hash,
    message::{v0, Message as LegacyMessage},
    pubkey::Pubkey,
    signature::Signature,
    transaction::{self, uses_durable_nonce, Transaction, VersionedTransaction},
};
use std::{sync::RwLock, time::Duration};

use crate::rpc_sender::RpcSender;

// -----------------
// RpcClientConfig
// -----------------
#[derive(Default)]
pub struct RpcClientConfig {
    pub commitment_config: CommitmentConfig,
    pub confirm_transaction_initial_timeout: Option<Duration>,
}

impl RpcClientConfig {
    pub fn with_commitment(commitment_config: CommitmentConfig) -> Self {
        RpcClientConfig {
            commitment_config,
            ..Self::default()
        }
    }
}

// -----------------
// RpcClient
// -----------------
pub struct RpcClient {
    sender: Box<dyn RpcSender + Send + Sync + 'static>,
    config: RpcClientConfig,
    node_version: RwLock<Option<semver::Version>>,
}

impl RpcClient {
    /// Create an `RpcClient` from an [`RpcSender`] and an [`RpcClientConfig`].
    ///
    /// This is the basic constructor, allowing construction with any type of
    /// `RpcSender`. Most applications should use one of the other constructors,
    /// such as [`RpcClient::new`], [`RpcClient::new_with_commitment`] or
    /// [`RpcClient::new_with_timeout`].
    pub fn new_sender<T: RpcSender + Send + Sync + 'static>(
        sender: T,
        config: RpcClientConfig,
    ) -> Self {
        Self {
            sender: Box::new(sender),
            node_version: RwLock::new(None),
            config,
        }
    }

    /// Create an HTTP `RpcClient` with specified [commitment level][cl].
    ///
    /// [cl]: https://solana.com/docs/rpc#configuring-state-commitment
    ///
    /// The URL is an HTTP URL, usually for port 8899, as in
    /// "http://localhost:8899".
    ///
    /// The client has a default timeout of 30 seconds, and a user-specified
    /// [`CommitmentLevel`] via [`CommitmentConfig`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use solana_sdk::commitment_config::CommitmentConfig;
    /// # use solana_rpc_client::nonblocking::rpc_client::RpcClient;
    /// let url = "http://localhost:8899".to_string();
    /// let commitment_config = CommitmentConfig::processed();
    /// let client = RpcClient::new_with_commitment(url, commitment_config);
    /// ```
    pub fn new_with_commitment(url: String, commitment_config: CommitmentConfig) -> Self {
        Self::new_sender(
            HttpSender::new(url),
            RpcClientConfig::with_commitment(commitment_config),
        )
    }
}
