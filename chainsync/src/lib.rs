#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![doc = include_str!("../README.md")]

mod account;
mod acquisition;
mod aml;
mod ata;
mod delegation;
mod grpc;
mod http;
mod metrics;
mod program;
mod rpc;
mod websocket;
mod worker;

use std::{borrow::Borrow, sync::Arc};

use engine::{Engine, EngineError};
use nucleus::{
    runtime::TransactionView,
    shutdown::{Service, ShutdownManager},
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use url::Url;

use account::AccountSubscription;
pub use account::{AccountProperty, ChainSyncAccount, SyncTarget};
use acquisition::Acquisition;

use crate::http::Fetcher;
use crate::websocket::Pool;

pub use aml::{Config as AmlConfig, Error as AmlError};
pub use grpc::{Error as GrpcError, StreamConfig as GrpcStreamConfig};
pub use http::Error as HttpError;
pub use rpc::{DecodeError, Error as RpcError};
pub use websocket::{
    Config as WebSocketConfig, Error as WebSocketError, Provider as WebSocketProvider,
};

/// Provider configuration for HTTP fetches and live WebSocket/gRPC updates.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ChainSyncConfig {
    /// Checks each distinct post-delegation action signer; `None` disables assessment.
    pub aml: Option<AmlConfig>,
    /// HTTP account providers on the same base chain; must be nonempty.
    pub http: Vec<Url>,
    /// WebSocket subscription providers.
    pub websocket: WebSocketConfig,
    /// Yellowstone-compatible streams for delegation events and subscription redundancy.
    /// Must contain at least one stream.
    pub grpc: Vec<GrpcStreamConfig>,
}

/// Acquires missing base-chain accounts for Engine and applies live provider updates.
pub struct ChainSync {
    aml: Option<aml::Client>,
    engine: Engine,
    fetcher: Fetcher,
    websocket: Pool,
    /// Keeps all gRPC streams alive for the synchronizer's lifetime.
    grpc: Vec<grpc::Client>,
}

/// Failure to acquire or apply base-chain account state.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("at least one gRPC lifecycle stream is required")]
    MissingGrpc,
    #[error("HTTP account fetch failed: {0}")]
    Fetch(#[from] HttpError),
    #[error("WebSocket subscription failed: {0}")]
    Subscribe(#[from] WebSocketError),
    #[error("gRPC operation failed: {0}")]
    Grpc(#[from] GrpcError),
    #[error("Engine account operation failed: {0}")]
    Engine(#[from] EngineError),
    #[error("invalid program: {0}")]
    Program(&'static str),
    #[error("invalid ProgramData: {0}")]
    ProgramData(#[from] Box<bincode::ErrorKind>),
    #[error("invalid delegation record: {0}")]
    Record(&'static str),
    #[error("invalid delegation actions: {0}")]
    Actions(#[from] borsh::io::Error),
    #[error("delegation action decryption failed: {0}")]
    Decrypt(#[from] delegation_program_api::decrypt::DecryptError),
    #[error("delegation signer assessment failed: {0}")]
    Aml(#[from] AmlError),
}

/// Result of a synchronization operation.
pub type Result<T> = std::result::Result<T, Error>;

impl ChainSync {
    /// Starts account synchronization services under the supplied shutdown manager.
    ///
    /// Only one synchronizer may own an Engine's cache-eviction receiver; construction
    /// fails if it is already taken. Provider connections start in the background,
    /// so success does not mean they are ready for [`Self::sync`].
    /// See [`ChainSyncConfig`] for provider requirements.
    pub fn new(
        engine: Engine,
        config: ChainSyncConfig,
        shutdown: &mut ShutdownManager,
    ) -> Result<Arc<Self>> {
        if config.grpc.is_empty() {
            return Err(Error::MissingGrpc);
        }
        metrics::init();
        let aml = config.aml.map(aml::Client::new).transpose()?;
        let (websocket, websocket_rx) = Pool::new(config.websocket, shutdown);
        let fetcher = Fetcher::new(config.http, engine.clone())?;
        let (events, grpc_rx) = mpsc::channel(8192);
        let mut grpc = Vec::with_capacity(config.grpc.len());
        for (id, stream) in config.grpc.into_iter().enumerate() {
            let client = grpc::Client::new(id, stream, engine.clone(), events.clone(), shutdown)?;
            grpc.push(client);
        }
        let chain_sync = Arc::new(Self {
            aml,
            engine,
            fetcher,
            websocket,
            grpc,
        });
        let evictions =
            chain_sync.engine.accounts().subscribe_evictions().map_err(EngineError::from)?;
        let shutdown = shutdown.handle(Service::ChainSyncWorker);
        tokio::spawn(Self::run(
            Arc::clone(&chain_sync),
            websocket_rx,
            grpc_rx,
            evictions,
            shutdown,
        ));
        Ok(chain_sync)
    }

    /// Classifies a sanitized transaction's static accounts and ensures they are resident.
    ///
    /// The payer takes precedence over invoked programs, then requested writable
    /// accounts. This classifies acquisition roles, not execution writability;
    /// Engine resolves execution permissions when processing the transaction.
    pub async fn ensure_transaction_accounts(&self, transaction: &TransactionView) -> Result<u64> {
        let signed = usize::from(transaction.num_required_signatures());
        let writable_signed =
            signed - usize::from(transaction.num_readonly_signed_static_accounts());
        let writable_end = transaction.static_account_keys().len()
            - usize::from(transaction.num_readonly_unsigned_static_accounts());
        let requests =
            transaction.static_account_keys().iter().enumerate().map(|(index, &pubkey)| {
                let property = if index == 0 {
                    AccountProperty::Payer
                } else if transaction
                    .instructions_iter()
                    .any(|ix| usize::from(ix.program_id_index) == index)
                {
                    AccountProperty::Program
                } else if index < writable_signed || (index >= signed && index < writable_end) {
                    AccountProperty::Writable
                } else {
                    AccountProperty::Readonly
                };
                ChainSyncAccount { pubkey, property }
            });
        self.ensure(requests).await
    }

    /// Selects missing accounts under ordered Engine leases, then synchronizes them.
    ///
    /// Resident accounts are left unchanged and gain no new subscriptions.
    /// [`AccountProperty`] determines which missing accounts receive live updates;
    /// subscriptions precede the initial HTTP fetch. Required ProgramData, delegation
    /// records, and post-delegation action dependencies are acquired automatically.
    ///
    /// Delegation requires DLP-owned account and record images with an accepted
    /// authority. This Engine's authority produces a delegated account; the default
    /// authority produces a confined `Magic` account. Funded ordinary images
    /// and executable images enter in `ReadOnly` mode; absent accounts remain
    /// `Uninit` at the response context slot. Canonical token ATAs may project a
    /// locally delegated eATA balance; its raw eATA is retained readonly.
    ///
    /// Success includes dependency acquisition and post-delegation action handling,
    /// but does not imply completion of any scheduled on-chain rescue. Batches can
    /// observe different slots, and errors do not roll back earlier materializations.
    ///
    /// Returns the number of account entries fetched over HTTP, including companions,
    /// action dependencies, and null results. Refetches in later acquisition waves
    /// count again; provider retries do not. Empty or entirely resident requests
    /// return zero. Background stream work and AML requests are excluded;
    /// errors do not expose a partial count.
    ///
    /// # Input requirements
    ///
    /// Writable and program pubkeys must be unique; repeated payer and read-only
    /// requests are collapsed. Requested accounts must not overlap a program's
    /// derived ProgramData address.
    pub async fn ensure<I>(&self, requests: I) -> Result<u64>
    where
        I: IntoIterator,
        I::Item: Borrow<ChainSyncAccount>,
    {
        let accounts = requests.into_iter().map(|account| *account.borrow()).collect();
        self.sync(self.missing(accounts).await?, None).await
    }

    /// Synchronizes selected targets, whether absent or eligible resident state.
    ///
    /// Targets must be unique leases from this Engine, acquired in pubkey order.
    /// Do not supply a program together with its derived ProgramData address.
    /// The caller establishes resident replacement eligibility under those leases.
    /// Engine still enforces authority, mode transitions, and slot ordering.
    ///
    /// Subscriptions precede HTTP fetches. Companion fetches retain the target lease;
    /// all target leases are released before acquiring post-delegation dependencies.
    /// `min_context_slot` is a response freshness floor, never a replacement eligibility rule.
    /// Returns fetched HTTP entries, including companions, nulls, and dependency waves.
    /// Errors may leave earlier targets materialized; they do not roll back the batch.
    pub async fn sync(
        &self,
        targets: Vec<SyncTarget<'_>>,
        min_context_slot: Option<u64>,
    ) -> Result<u64> {
        Acquisition::new(self, min_context_slot).run(targets).await
    }

    /// Selects only accounts still missing after Engine's ordered lease acquisition.
    async fn missing(&self, mut requests: Vec<ChainSyncAccount>) -> Result<Vec<SyncTarget<'_>>> {
        requests.sort_unstable_by_key(|request| request.pubkey);
        requests.dedup_by_key(|request| request.pubkey);
        let keys: Vec<_> = requests.iter().map(|request| request.pubkey).collect();
        let mut accessors = self.engine.missing_accounts(&keys).await?.into_iter().peekable();
        let mut targets = Vec::with_capacity(accessors.len());
        for request in requests {
            if let Some(accessor) =
                accessors.next_if(|accessor| accessor.pubkey() == request.pubkey)
            {
                targets.push(SyncTarget::new(accessor, request.property));
            }
        }
        Ok(targets)
    }
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

// The shared fixtures import the same API in library and integration tests.
#[cfg(test)]
use crate as chainsync;
