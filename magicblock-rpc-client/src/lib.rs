#![allow(deprecated)]

mod signature_confirmer;
pub mod utils;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use arc_swap::ArcSwapOption;
use futures_util::future::try_join_all;
use serde_json::json;
use signature_confirmer::{SignatureConfirmer, SignatureConfirmerConfig};
use solana_account::Account;
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_address_lookup_table_interface::state::{
    AddressLookupTable, LookupTableMeta,
};
use solana_clock::Slot;
use solana_commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_hash::Hash;
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::{
    client_error::{Error as RpcClientError, ErrorKind as RpcClientErrorKind},
    config::{
        RpcAccountInfoConfig, RpcSendTransactionConfig, RpcTransactionConfig,
    },
    request::{RpcError, RpcRequest},
    response::{Response, RpcBlockhash},
};
use solana_signature::Signature;
use solana_transaction_error::{TransactionError, TransactionResult};
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, UiTransactionEncoding,
};
use tokio::sync::Mutex as TMutex;
use tokio_util::sync::CancellationToken;
use tracing::*;

/// The encoding to use when sending transactions
pub const SEND_TRANSACTION_ENCODING: UiTransactionEncoding =
    UiTransactionEncoding::Base64;

/// The configuration to use when sending transactions
pub const SEND_TRANSACTION_CONFIG: RpcSendTransactionConfig =
    RpcSendTransactionConfig {
        preflight_commitment: None,
        skip_preflight: true,
        encoding: Some(SEND_TRANSACTION_ENCODING),
        max_retries: None,
        min_context_slot: None,
    };

// -----------------
// MagicBlockRpcClientError
// -----------------
#[derive(Debug, thiserror::Error)]
pub enum MagicBlockRpcClientError {
    #[error("RPC Client error: {0}")]
    RpcClientError(Box<solana_rpc_client_api::client_error::Error>),

    #[error("Error getting blockhash: {0} ({0:?})")]
    GetLatestBlockhash(Box<solana_rpc_client_api::client_error::Error>),

    #[error("Error getting slot: {0} ({0:?})")]
    GetSlot(Box<solana_rpc_client_api::client_error::Error>),

    #[error("Error deserializing lookup table: {0}")]
    LookupTableDeserialize(InstructionError),

    #[error("Error sending transaction: {0} ({0:?})")]
    SendTransaction(Box<solana_rpc_client_api::client_error::Error>),

    #[error("Error getting signature status for: {0} {1}")]
    CannotGetTransactionSignatureStatus(Signature, String),

    #[error(
        "Error confirming signature status of {0} at desired commitment level {1}"
    )]
    CannotConfirmTransactionSignatureStatus(Signature, CommitmentLevel),

    #[error("Sent transaction {1} but got error: {0:?}")]
    SentTransactionError(TransactionError, Signature),
}

impl From<solana_rpc_client_api::client_error::Error>
    for MagicBlockRpcClientError
{
    fn from(e: solana_rpc_client_api::client_error::Error) -> Self {
        Self::RpcClientError(Box::new(e))
    }
}

impl MagicBlockRpcClientError {
    /// Returns the signature of the transaction that caused the error
    /// if available.
    pub fn signature(&self) -> Option<Signature> {
        use MagicBlockRpcClientError::*;
        match self {
            CannotGetTransactionSignatureStatus(sig, _)
            | SentTransactionError(_, sig)
            | CannotConfirmTransactionSignatureStatus(sig, _) => Some(*sig),
            _ => None,
        }
    }
}

pub type MagicBlockRpcClientResult<T> =
    std::result::Result<T, MagicBlockRpcClientError>;

// -----------------
// SendAndConfirmTransaction Config and Outcome
// -----------------
pub enum MagicBlockSendTransactionConfig {
    /// Just send the transaction and return the signature.
    Send,
    /// Send a transaction and confirm it with the given parameters.
    SendAndConfirm {
        /// If provided we will wait for the given blockhash to become valid if
        /// getting the signature status fails due to `BlockhashNotFound`.
        wait_for_blockhash_to_become_valid: Option<Duration>,
        /// If provided we will try multiple time so find the signature status
        /// of the transaction at the 'processed' level even if the recent blockhash
        /// already became valid.
        wait_for_processed_level: Option<Duration>,
        /// How long to wait in between checks for processed commitment level.
        check_for_processed_interval: Option<Duration>,
        /// If provided it will wait for the transaction to be committed at the given
        /// commitment level. If not we just wait for the transaction to be processed and
        /// return the processed status.
        wait_for_commitment_level: Option<Duration>,
        /// How long to wait in between checks for desired commitment level.
        check_for_commitment_interval: Option<Duration>,
    },
}

// This seems rather large, but if we pick a lower value then test fail locally running
// against a (busy) solana test validator
// I verified that it actually takes this long for the transaction to become available
// in the explorer. Power settings on my machine actually affect this behavior.
const DEFAULT_MAX_TIME_TO_PROCESSED: Duration = Duration::from_millis(50_000);

impl MagicBlockSendTransactionConfig {
    // This will be used if we change the strategy for reallocs or writes
    #[allow(dead_code)]
    pub fn ensure_sent() -> Self {
        Self::Send
    }

    pub fn ensure_processed() -> Self {
        Self::SendAndConfirm {
            wait_for_blockhash_to_become_valid: Some(Duration::from_millis(
                2_000,
            )),
            wait_for_processed_level: Some(DEFAULT_MAX_TIME_TO_PROCESSED),
            check_for_processed_interval: Some(Duration::from_millis(400)),
            wait_for_commitment_level: None,
            check_for_commitment_interval: None,
        }
    }

    pub fn ensure_committed() -> Self {
        Self::SendAndConfirm {
            wait_for_blockhash_to_become_valid: Some(Duration::from_millis(
                2_000,
            )),
            wait_for_processed_level: Some(DEFAULT_MAX_TIME_TO_PROCESSED),
            check_for_processed_interval: Some(Duration::from_millis(400)),
            // NOTE: that this time is after we already verified that the transaction was
            //       processed
            wait_for_commitment_level: Some(Duration::from_millis(8_000)),
            check_for_commitment_interval: Some(Duration::from_millis(400)),
        }
    }

    pub fn ensures_committed(&self) -> bool {
        use MagicBlockSendTransactionConfig::*;
        match self {
            Send => false,
            SendAndConfirm {
                wait_for_commitment_level,
                ..
            } => wait_for_commitment_level.is_some(),
        }
    }
}

#[derive(Debug)]
pub struct MagicBlockSendTransactionOutcome {
    signature: Signature,
    processed_err: Option<TransactionError>,
    confirmed_err: Option<TransactionError>,
}

impl MagicBlockSendTransactionOutcome {
    pub fn into_signature(self) -> Signature {
        self.signature
    }

    pub fn into_signature_and_error(
        self,
    ) -> (Signature, Option<TransactionError>) {
        (self.signature, self.confirmed_err.or(self.processed_err))
    }

    /// Returns the error that occurred when processing the transaction.
    /// NOTE: this is never set if we use the [MagicBlockSendConfig::Send] option.
    pub fn error(&self) -> Option<&TransactionError> {
        self.confirmed_err.as_ref().or(self.processed_err.as_ref())
    }

    pub fn into_error(self) -> Option<TransactionError> {
        self.confirmed_err.or(self.processed_err)
    }

    pub fn into_result(self) -> Result<Signature, MagicBlockRpcClientError> {
        if let Some(err) = self.confirmed_err.or(self.processed_err) {
            Err(MagicBlockRpcClientError::SentTransactionError(
                err,
                self.signature,
            ))
        } else {
            Ok(self.signature)
        }
    }
}

// -----------------
// MagicBlockRpcClient
// -----------------

// Derived from error from helius RPC: Failed to download accounts: Error { request: Some(GetMultipleAccounts), kind: RpcError(RpcResponseError { code: -32602, message: "Too many inputs provided; max 100", data: Empty }) }
const MAX_MULTIPLE_ACCOUNTS: usize = 100;
// Keep this below the default 50ms * 150 slot blockhash lifetime.
const BLOCKHASH_CACHE_TTL: Duration = Duration::from_secs(5);
const SLOT_CACHE_TTL: Duration = Duration::from_millis(400);

#[derive(Default)]
struct RpcClientCache {
    blockhash: TMutex<BlockhashCache>,
    slot: TMutex<Option<CachedSlot>>,
}

#[derive(Default)]
struct BlockhashCache {
    latest: Option<CachedBlockhash>,
    recent: HashMap<Hash, CachedBlockhash>,
}

#[derive(Clone, Copy, Debug)]
struct CachedBlockhash {
    pub blockhash: Hash,
    context_slot: Slot,
    last_valid_block_height: u64,
    fetched_at: Instant,
}

/// Process-wide, shared source of truth for the latest base-layer
/// blockhash. A single background task writes it; all consumers read it
/// lock-free. Cloning is cheap (Arc bump) and all clones share state.
#[derive(Clone, Default, Debug)]
pub struct BaseLayerBlockhash {
    inner: Arc<ArcSwapOption<CachedBlockhash>>,
}

impl BaseLayerBlockhash {
    /// Lock-free read of the cached blockhash, if the refresher has
    /// populated it at least once and it hasn't been invalidated.
    fn get(&self) -> Option<CachedBlockhash> {
        self.inner.load().as_deref().copied()
    }

    /// Returns the cached base-layer blockhash, if available.
    pub fn latest_blockhash(&self) -> Option<Hash> {
        self.get().map(|cached| cached.blockhash)
    }

    fn store(&self, value: CachedBlockhash) {
        self.inner.store(Some(Arc::new(value)));
    }

    /// Drop the cached value so the next read falls back to a direct
    /// fetch and the next refresh tick re-populates. Called by retry
    /// paths after a transaction is rejected for a stale blockhash.
    pub fn invalidate(&self) {
        self.inner.store(None);
    }

    /// Primes the cache synchronously (so the first reader never races an
    /// empty cache) then spawns the single background refresher.
    pub async fn spawn_blockhash_refresher(
        &self,
        rpc_client: Arc<RpcClient>,
        cancel: CancellationToken,
    ) {
        if let Err(err) = self.refresh_once(&rpc_client).await {
            warn!(error = ?err, "Initial base-layer blockhash fetch failed");
        }
        let cache = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REFRESH_INTERVAL);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(err) = cache.refresh_once(&rpc_client).await {
                            warn!(error = ?err, "Failed to refresh base-layer blockhash");
                        }
                    }
                    _ = cancel.cancelled() => break,
                }
            }
        });
    }

    async fn refresh_once(
        &self,
        rpc_client: &RpcClient,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let resp: Response<RpcBlockhash> = rpc_client
            .send(
                RpcRequest::GetLatestBlockhash,
                json!([rpc_client.commitment()]),
            )
            .await
            .map_err(|e| {
                MagicBlockRpcClientError::GetLatestBlockhash(Box::new(e))
            })?;
        let blockhash = resp.value.blockhash.parse().map_err(|_| {
            MagicBlockRpcClientError::GetLatestBlockhash(Box::new(
                RpcClientError::new_with_request(
                    RpcClientErrorKind::RpcError(RpcError::ParseError(
                        "Hash".to_string(),
                    )),
                    RpcRequest::GetLatestBlockhash,
                ),
            ))
        })?;

        self.store(CachedBlockhash {
            blockhash,
            last_valid_block_height: resp.value.last_valid_block_height,
            context_slot: resp.context.slot,
            fetched_at: Instant::now(),
        });

        Ok(())
    }
}

const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
struct CachedSlot {
    slot: Slot,
    fetched_at: Instant,
}

/// Wraps a [RpcClient] to provide improved functionality, specifically
/// for sending transactions.
#[derive(Clone)]
pub struct MagicblockRpcClient {
    client: Arc<RpcClient>,
    cache: Arc<RpcClientCache>,
    chain_slot: Option<Arc<AtomicU64>>,
    confirmer: Arc<SignatureConfirmer>,
    blockhash_registry: Option<BaseLayerBlockhash>,
}

impl From<RpcClient> for MagicblockRpcClient {
    fn from(client: RpcClient) -> Self {
        Self::new(Arc::new(client))
    }
}

impl MagicblockRpcClient {
    /// Create a new [MagicBlockRpcClient] from an existing [RpcClient].
    pub fn new(client: Arc<RpcClient>) -> Self {
        Self::new_with_options(client, None, None, None)
    }

    pub fn new_with_chain_slot(
        client: Arc<RpcClient>,
        chain_slot: Arc<AtomicU64>,
    ) -> Self {
        Self::new_with_options(client, Some(chain_slot), None, None)
    }

    pub fn new_with_websocket(
        client: Arc<RpcClient>,
        websocket_url: Option<String>,
    ) -> Self {
        Self::new_with_options(client, None, websocket_url, None)
    }

    pub fn new_with_chain_slot_and_websocket(
        client: Arc<RpcClient>,
        chain_slot: Arc<AtomicU64>,
        websocket_url: Option<String>,
    ) -> Self {
        Self::new_with_options(client, Some(chain_slot), websocket_url, None)
    }

    pub fn new_with_registry(
        client: Arc<RpcClient>,
        chain_slot: Option<Arc<AtomicU64>>,
        websocket_url: Option<String>,
        blockhash_registry: Option<BaseLayerBlockhash>,
    ) -> Self {
        Self::new_with_options(
            client,
            chain_slot,
            websocket_url,
            blockhash_registry,
        )
    }

    fn new_with_options(
        client: Arc<RpcClient>,
        chain_slot: Option<Arc<AtomicU64>>,
        websocket_url: Option<String>,
        blockhash_registry: Option<BaseLayerBlockhash>,
    ) -> Self {
        let confirmer = Arc::new(SignatureConfirmer::new(
            client.clone(),
            SignatureConfirmerConfig::with_websocket_url(websocket_url),
        ));
        Self {
            client,
            cache: Arc::new(RpcClientCache::default()),
            chain_slot,
            confirmer,
            blockhash_registry,
        }
    }

    pub async fn get_latest_blockhash(
        &self,
    ) -> MagicBlockRpcClientResult<Hash> {
        // 1. Shared registry: lock-free, no RPC.
        if let Some(ref bh_registry) = self.blockhash_registry {
            if let Some(cached) = bh_registry.get() {
                self.record_observed_slot(cached.context_slot).await;

                let mut local = self.cache.blockhash.lock().await;
                Self::cache_blockhash(&mut local, cached);
                return Ok(cached.blockhash);
            }
        }

        // 2. Legacy per-client cache + direct fetch
        let mut cached = self.cache.blockhash.lock().await;
        if let Some(blockhash) = Self::fresh_cached_blockhash(&cached) {
            return Ok(blockhash);
        }

        let (blockhash, context_slot, last_valid_block_height) =
            self.fetch_latest_blockhash_with_context().await?;
        Self::cache_blockhash(
            &mut cached,
            CachedBlockhash {
                blockhash,
                context_slot,
                last_valid_block_height,
                fetched_at: Instant::now(),
            },
        );
        drop(cached);

        self.record_observed_slot(context_slot).await;

        Ok(blockhash)
    }

    async fn fetch_latest_blockhash_with_context(
        &self,
    ) -> MagicBlockRpcClientResult<(Hash, Slot, u64)> {
        let resp: Response<RpcBlockhash> = self
            .client
            .send(RpcRequest::GetLatestBlockhash, json!([self.commitment()]))
            .await
            .map_err(|e| {
                MagicBlockRpcClientError::GetLatestBlockhash(Box::new(e))
            })?;
        let blockhash = resp.value.blockhash.parse().map_err(|_| {
            MagicBlockRpcClientError::GetLatestBlockhash(Box::new(
                RpcClientError::new_with_request(
                    RpcClientErrorKind::RpcError(RpcError::ParseError(
                        "Hash".to_string(),
                    )),
                    RpcRequest::GetLatestBlockhash,
                ),
            ))
        })?;

        Ok((
            blockhash,
            resp.context.slot,
            resp.value.last_valid_block_height,
        ))
    }

    pub async fn get_slot(&self) -> MagicBlockRpcClientResult<Slot> {
        let slot = self.fetch_slot().await?;
        self.record_observed_slot(slot).await;
        Ok(slot)
    }

    async fn get_cached_slot(&self) -> MagicBlockRpcClientResult<Slot> {
        let mut cached = self.cache.slot.lock().await;
        if let Some(slot) = Self::fresh_cached_slot(&cached) {
            return Ok(slot);
        }

        let slot = self.fetch_slot().await?;
        Self::cache_slot(&mut cached, slot);
        drop(cached);

        self.update_chain_slot(slot);

        Ok(slot)
    }

    async fn fetch_slot(&self) -> MagicBlockRpcClientResult<Slot> {
        self.client
            .get_slot()
            .await
            .map_err(|e| MagicBlockRpcClientError::GetSlot(Box::new(e)))
    }

    pub async fn invalidate_cached_blockhash(&self) {
        if let Some(registry) = &self.blockhash_registry {
            registry.invalidate();
        }
        let mut cached = self.cache.blockhash.lock().await;
        cached.latest = None;
    }

    fn fresh_cached_blockhash(cached: &BlockhashCache) -> Option<Hash> {
        cached
            .latest
            .as_ref()
            .filter(|value| value.fetched_at.elapsed() < BLOCKHASH_CACHE_TTL)
            .map(|value| {
                trace!(
                    context_slot = value.context_slot,
                    last_valid_block_height = value.last_valid_block_height,
                    "Using cached latest blockhash"
                );
                value.blockhash
            })
    }

    fn cache_blockhash(
        cached: &mut BlockhashCache,
        blockhash: CachedBlockhash,
    ) {
        cached.latest = Some(blockhash);
        cached.recent.insert(blockhash.blockhash, blockhash);
        cached.recent.retain(|_, value| {
            value.fetched_at.elapsed() < DEFAULT_MAX_TIME_TO_PROCESSED
        });
    }

    async fn cached_blockhash_metadata(
        &self,
        blockhash: &Hash,
    ) -> Option<CachedBlockhash> {
        let cached = self.cache.blockhash.lock().await;
        cached.recent.get(blockhash).copied()
    }

    fn fresh_cached_slot(cached: &Option<CachedSlot>) -> Option<Slot> {
        cached
            .as_ref()
            .filter(|value| value.fetched_at.elapsed() < SLOT_CACHE_TTL)
            .map(|value| value.slot)
    }

    async fn record_observed_slot(&self, slot: Slot) {
        self.update_chain_slot(slot);
        let mut cached = self.cache.slot.lock().await;
        Self::cache_slot(&mut cached, slot);
    }

    fn cache_slot(cached: &mut Option<CachedSlot>, slot: Slot) {
        if cached.as_ref().is_none_or(|value| slot >= value.slot) {
            *cached = Some(CachedSlot {
                slot,
                fetched_at: Instant::now(),
            });
        }
    }

    fn observed_chain_slot(&self) -> Option<Slot> {
        self.chain_slot
            .as_ref()
            .map(|slot| slot.load(Ordering::Relaxed))
            .filter(|slot| *slot > 0)
    }

    fn update_chain_slot(&self, slot: Slot) {
        if let Some(chain_slot) = &self.chain_slot {
            chain_slot.fetch_max(slot, Ordering::Relaxed);
        }
    }

    pub async fn get_account(
        &self,
        pubkey: &Pubkey,
    ) -> MagicBlockRpcClientResult<Option<Account>> {
        let err = match self.client.get_account(pubkey).await {
            Ok(acc) => return Ok(Some(acc)),
            Err(err) => match err.kind() {
                RpcClientErrorKind::RpcError(rpc_err) => {
                    if let RpcError::ForUser(msg) = rpc_err {
                        if msg.starts_with("AccountNotFound") {
                            return Ok(None);
                        }
                    }
                    err
                }
                _ => err,
            },
        };
        Err(MagicBlockRpcClientError::RpcClientError(Box::new(err)))
    }
    pub async fn get_multiple_accounts(
        &self,
        pubkeys: &[Pubkey],
        max_per_fetch: Option<usize>,
    ) -> MagicBlockRpcClientResult<Vec<Option<Account>>> {
        self.get_multiple_accounts_with_commitment(
            pubkeys,
            self.commitment(),
            max_per_fetch,
        )
        .await
    }

    pub async fn get_multiple_accounts_with_commitment(
        &self,
        pubkeys: &[Pubkey],
        commitment: CommitmentConfig,
        max_per_fetch: Option<usize>,
    ) -> MagicBlockRpcClientResult<Vec<Option<Account>>> {
        self.get_multiple_accounts_with_config(
            pubkeys,
            RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64Zstd),
                commitment: Some(commitment),
                data_slice: None,
                min_context_slot: None,
            },
            max_per_fetch,
        )
        .await
    }

    pub async fn get_multiple_accounts_with_config(
        &self,
        pubkeys: &[Pubkey],
        config: RpcAccountInfoConfig,
        max_per_fetch: Option<usize>,
    ) -> MagicBlockRpcClientResult<Vec<Option<Account>>> {
        let max_per_fetch = max_per_fetch.unwrap_or(MAX_MULTIPLE_ACCOUNTS);
        let futs = pubkeys.chunks(max_per_fetch).map(|pubkey_chunk| {
            let config = config.clone();
            async move {
                self.client
                    .get_multiple_ui_accounts_with_config(pubkey_chunk, config)
                    .await
                    .map(|r| {
                        r.value
                            .into_iter()
                            .map(|opt| opt.and_then(|ui| ui.to_account()))
                            .collect::<Vec<_>>()
                    })
                    .map_err(MagicBlockRpcClientError::from)
            }
        });
        Ok(try_join_all(futs).await?.into_iter().flatten().collect())
    }

    pub async fn get_lookup_table_meta(
        &self,
        pubkey: &Pubkey,
    ) -> MagicBlockRpcClientResult<Option<LookupTableMeta>> {
        let acc = self.get_account(pubkey).await?;
        let Some(acc) = acc else { return Ok(None) };

        let table =
            AddressLookupTable::deserialize(&acc.data).map_err(|err| {
                MagicBlockRpcClientError::LookupTableDeserialize(err)
            })?;
        Ok(Some(table.meta))
    }

    pub async fn get_lookup_table_addresses(
        &self,
        pubkey: &Pubkey,
    ) -> MagicBlockRpcClientResult<Option<Vec<Pubkey>>> {
        let acc = self.get_account(pubkey).await?;
        let Some(acc) = acc else { return Ok(None) };

        let table =
            AddressLookupTable::deserialize(&acc.data).map_err(|err| {
                MagicBlockRpcClientError::LookupTableDeserialize(err)
            })?;
        Ok(Some(table.addresses.to_vec()))
    }

    pub async fn request_airdrop(
        &self,
        pubkey: &Pubkey,
        lamports: u64,
    ) -> MagicBlockRpcClientResult<Signature> {
        self.client
            .request_airdrop(pubkey, lamports)
            .await
            .map_err(|e| MagicBlockRpcClientError::RpcClientError(Box::new(e)))
    }

    pub fn commitment(&self) -> CommitmentConfig {
        self.client.commitment()
    }

    pub fn commitment_level(&self) -> CommitmentLevel {
        self.commitment().commitment
    }

    pub async fn wait_for_next_slot(&self) -> MagicBlockRpcClientResult<Slot> {
        let slot = if let Some(slot) = self.observed_chain_slot() {
            slot
        } else {
            self.get_cached_slot().await?
        };
        self.wait_for_higher_slot(slot).await
    }

    pub async fn wait_for_higher_slot(
        &self,
        slot: Slot,
    ) -> MagicBlockRpcClientResult<Slot> {
        let higher_slot = loop {
            let next_slot = if let Some(next_slot) = self.observed_chain_slot()
            {
                next_slot
            } else {
                self.get_cached_slot().await?
            };
            if next_slot > slot {
                break next_slot;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };

        Ok(higher_slot)
    }

    /// Sends a transaction skipping preflight checks and then attempts to confirm
    /// it if so configured
    /// To confirm a transaction it uses the `client.commitment()` when waiting
    /// for a signature status.
    ///
    /// Does not support:
    /// - durable nonce transactions
    pub async fn send_transaction(
        &self,
        tx: &impl SerializableTransaction,
        config: &MagicBlockSendTransactionConfig,
    ) -> MagicBlockRpcClientResult<MagicBlockSendTransactionOutcome> {
        let sig = self
            .client
            .send_transaction_with_config(tx, SEND_TRANSACTION_CONFIG)
            .await
            .map_err(|e| {
                MagicBlockRpcClientError::SendTransaction(Box::new(e))
            })?;

        let MagicBlockSendTransactionConfig::SendAndConfirm {
            wait_for_processed_level,
            check_for_processed_interval,
            wait_for_blockhash_to_become_valid,
            wait_for_commitment_level,
            check_for_commitment_interval,
        } = config
        else {
            return Ok(MagicBlockSendTransactionOutcome {
                signature: sig,
                processed_err: None,
                confirmed_err: None,
            });
        };

        // 1. Wait for processed status
        let check_for_processed_interval = check_for_processed_interval
            .unwrap_or_else(|| Duration::from_millis(200));
        let wait_for_processed_level =
            wait_for_processed_level.unwrap_or_default();
        let processed_status = self
            .wait_for_processed_status(
                &sig,
                tx.get_recent_blockhash(),
                wait_for_processed_level,
                check_for_processed_interval,
                wait_for_blockhash_to_become_valid,
            )
            .await?;

        if let Err(err) = processed_status {
            return Err(MagicBlockRpcClientError::SentTransactionError(
                err, sig,
            ));
        }

        // 2. Wait for confirmed status if configured
        let confirmed_status = if let Some(wait_for_commitment_level) =
            wait_for_commitment_level
        {
            Some(
                self.wait_for_confirmed_status(
                    &sig,
                    wait_for_commitment_level,
                    check_for_commitment_interval,
                )
                .await?,
            )
        } else {
            None
        };

        Ok(MagicBlockSendTransactionOutcome {
            signature: sig,
            processed_err: processed_status.err(),
            confirmed_err: confirmed_status.and_then(|status| status.err()),
        })
    }

    /// Waits for a transaction to reach processed status
    #[instrument(
        skip(self),
        fields(
            signature = %signature,
            blockhash = %recent_blockhash,
            timeout_ms = timeout.as_millis() as u64,
        )
    )]
    pub async fn wait_for_processed_status(
        &self,
        signature: &Signature,
        recent_blockhash: &Hash,
        timeout: Duration,
        check_interval: Duration,
        _blockhash_valid_timeout: &Option<Duration>,
    ) -> MagicBlockRpcClientResult<TransactionResult<()>> {
        if let Some(status) = Box::pin(self.confirmer.wait_for_status(
            signature,
            CommitmentConfig::processed(),
            timeout,
            check_interval,
        ))
        .await
        {
            return Ok(status);
        }

        let blockhash_message = match (
            self.cached_blockhash_metadata(recent_blockhash).await,
            self.observed_chain_slot(),
        ) {
            (Some(blockhash), Some(slot))
                if slot <= blockhash.last_valid_block_height =>
            {
                "timed out while blockhash was still within observed slot window"
            }
            (Some(_), Some(_)) => {
                "timed out waiting for processed signature status"
            }
            _ => "timed out waiting for processed signature status",
        };

        Err(
            MagicBlockRpcClientError::CannotGetTransactionSignatureStatus(
                *signature,
                blockhash_message.to_string(),
            ),
        )
    }

    /// Waits for a transaction to reach confirmed status
    pub async fn wait_for_confirmed_status(
        &self,
        signature: &Signature,
        timeout: &Duration,
        check_interval: &Option<Duration>,
    ) -> MagicBlockRpcClientResult<TransactionResult<()>> {
        let check_interval =
            check_interval.unwrap_or_else(|| Duration::from_millis(200));

        if let Some(status) = Box::pin(self.confirmer.wait_for_status(
            signature,
            self.client.commitment(),
            *timeout,
            check_interval,
        ))
        .await
        {
            return Ok(status);
        }

        Err(
            MagicBlockRpcClientError::CannotConfirmTransactionSignatureStatus(
                *signature,
                self.client.commitment().commitment,
            ),
        )
    }

    pub async fn get_transaction(
        &self,
        signature: &Signature,
        config: Option<RpcTransactionConfig>,
    ) -> MagicBlockRpcClientResult<EncodedConfirmedTransactionWithStatusMeta>
    {
        let config = config.unwrap_or_else(|| RpcTransactionConfig {
            commitment: Some(self.commitment()),
            ..Default::default()
        });
        self.client
            .get_transaction_with_config(signature, config)
            .await
            .map_err(|e| MagicBlockRpcClientError::RpcClientError(Box::new(e)))
    }

    pub fn get_logs_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<Vec<String>> {
        tx.transaction.meta.as_ref()?.log_messages.clone().into()
    }

    pub async fn get_transaction_logs(
        &self,
        signature: &Signature,
        config: Option<RpcTransactionConfig>,
    ) -> MagicBlockRpcClientResult<Option<Vec<String>>> {
        let tx = self.get_transaction(signature, config).await?;
        Ok(Self::get_logs_from_transaction(&tx))
    }

    pub fn get_cus_from_transaction(
        tx: &EncodedConfirmedTransactionWithStatusMeta,
    ) -> Option<u64> {
        tx.transaction
            .meta
            .as_ref()?
            .compute_units_consumed
            .clone()
            .into()
    }

    pub async fn get_transaction_cus(
        &self,
        signature: &Signature,
        config: Option<RpcTransactionConfig>,
    ) -> MagicBlockRpcClientResult<Option<u64>> {
        let tx = self.get_transaction(signature, config).await?;
        Ok(Self::get_cus_from_transaction(&tx))
    }

    pub fn get_inner(&self) -> &Arc<RpcClient> {
        &self.client
    }
}
