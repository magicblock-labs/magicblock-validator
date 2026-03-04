use std::{
    collections::HashMap,
    mem,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use dlp::{
    delegation_metadata_seeds_from_delegated_account, state::DelegationMetadata,
};
use lru::LruCache;
use magicblock_metrics::metrics;
use magicblock_rpc_client::{MagicBlockRpcClientError, MagicblockRpcClient};
use solana_account::Account;
use solana_account_decoder::UiAccountEncoding;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    client_error::ErrorKind, config::RpcAccountInfoConfig,
    custom_error::JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED,
    request::RpcError,
};
use solana_signature::Signature;
use tokio::sync::{Mutex as TMutex, MutexGuard};
use tracing::{error, info, warn};

const NUM_FETCH_RETRIES: NonZeroUsize = NonZeroUsize::new(5).unwrap();
const MUTEX_POISONED_MSG: &str = "CacheTaskInfoFetcher mutex poisoned!";

#[async_trait]
pub trait TaskInfoFetcher: Send + Sync + 'static {
    /// Fetches correct commit nonces for pubkeys
    /// Those nonces can be used as correct commit_nonce during Commit
    async fn fetch_next_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>>;

    /// Fetches current commit nonces for pubkeys
    /// Missing nonces will be fetched from chain
    async fn fetch_current_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>>;

    /// Fetches rent reimbursement address for pubkeys
    async fn fetch_rent_reimbursements(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<Vec<Pubkey>>;

    /// Peeks current commit ids for pubkeys
    async fn peek_commit_nonce(&self, pubkey: &Pubkey) -> Option<u64>;

    /// Resets cache for some or all accounts
    fn reset(&self, reset_type: ResetType);

    async fn get_base_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>>;
}

pub enum ResetType<'a> {
    All,
    Specific(&'a [Pubkey]),
}

type NonceLock = Arc<TMutex<u64>>;

struct CacheInner {
    active: LruCache<Pubkey, NonceLock>,
    retiring: HashMap<Pubkey, NonceLock>,
}

struct CacheInnerGuard<'a> {
    inner: &'a Mutex<CacheInner>,
    nonce_locks: Vec<(Pubkey, NonceLock)>,
}

impl<'a> CacheInnerGuard<'a> {
    // Acquire per-account locks sequentially in sorted order (see sort above).
    // join_all would poll all futures concurrently, allowing partial acquisition
    // and producing the classic A→B / B→A deadlock across concurrent callers.
    async fn lock<'s>(&'s self) -> Vec<(&'s Pubkey, MutexGuard<'s, u64>)> {
        let mut output = Vec::with_capacity(self.nonce_locks.len());
        for (pubkey, lock) in self.nonce_locks.iter() {
            let guard = lock.lock().await;
            output.push((pubkey, guard))
        }

        output
    }
}

impl<'a> Drop for CacheInnerGuard<'a> {
    fn drop(&mut self) {
        let mut inner = self.inner.lock().expect(MUTEX_POISONED_MSG);
        let nonce_locks = mem::take(&mut self.nonce_locks);
        for (pubkey, lock) in nonce_locks {
            // Drop our clone first so strong_count reflects only other
            // live holders when we check below
            drop(lock);
            let should_remove = inner
                .retiring
                .get(&pubkey)
                .is_some_and(|l| Arc::strong_count(l) == 1);
            if should_remove {
                inner.retiring.remove(&pubkey);
            }
        }
    }
}

impl CacheInner {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            active: LruCache::new(capacity),
            retiring: HashMap::new(),
        }
    }
}

pub struct CacheTaskInfoFetcher {
    rpc_client: MagicblockRpcClient,
    cache: Mutex<CacheInner>,
}

impl CacheTaskInfoFetcher {
    pub fn new(rpc_client: MagicblockRpcClient) -> Self {
        const CACHE_SIZE: NonZeroUsize = NonZeroUsize::new(1000).unwrap();

        Self {
            rpc_client,
            cache: Mutex::new(CacheInner::new(CACHE_SIZE)),
        }
    }

    pub fn with_capacity(
        capacity: NonZeroUsize,
        rpc_client: MagicblockRpcClient,
    ) -> Self {
        Self {
            rpc_client,
            cache: Mutex::new(CacheInner::new(capacity)),
        }
    }

    /// Fetches [`DelegationMetadata`]s with some num of retries
    pub async fn fetch_metadata_with_retries(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        max_retries: NonZeroUsize,
    ) -> TaskInfoFetcherResult<Vec<DelegationMetadata>> {
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        let pda_accounts: Vec<Pubkey> = pubkeys
            .iter()
            .map(|delegated_account| {
                Pubkey::find_program_address(
                    delegation_metadata_seeds_from_delegated_account!(
                        delegated_account
                    ),
                    &dlp::id(),
                )
                .0
            })
            .collect();

        let accounts = Self::fetch_accounts_with_retries(
            rpc_client,
            &pda_accounts,
            min_context_slot,
            max_retries,
        )
        .await?;

        accounts
            .into_iter()
            .zip(pda_accounts)
            .map(|(account, pda)| {
                DelegationMetadata::try_from_bytes_with_discriminator(
                    &account.data,
                )
                .map_err(|_| TaskInfoFetcherError::InvalidAccountDataError(pda))
            })
            .collect()
    }

    /// Fetches [`Account`]s with some num of retries
    pub async fn fetch_accounts_with_retries(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        max_retries: NonZeroUsize,
    ) -> TaskInfoFetcherResult<Vec<Account>> {
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        let mut i = 0;
        loop {
            i += 1;
            let err = match Self::fetch_accounts(
                rpc_client,
                pubkeys,
                min_context_slot,
            )
            .await
            {
                Ok(value) => break Ok(value),
                Err(err) => err,
            };

            match err {
                TaskInfoFetcherError::AccountNotFoundError(_) => {
                    break Err(err)
                }
                err @ TaskInfoFetcherError::InvalidAccountDataError(_) => {
                    error!(error = ?err, "Unexpected error");
                    break Err(err);
                }
                TaskInfoFetcherError::MinContextSlotNotReachedError(_, _) => {
                    // Get some extra sleep
                    info!(
                        min_context_slot,
                        attempt = i,
                        "Min context slot not reached"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                TaskInfoFetcherError::MagicBlockRpcClientError(ref err) => {
                    warn!(error = ?err, attempt = i, "Fetch account error");
                }
            }

            if i >= max_retries.get() {
                break Err(err);
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Fetches specified list of accounts
    pub async fn fetch_accounts(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<Vec<Account>> {
        // Early return if no pubkeys to process
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        metrics::inc_task_info_fetcher_a_count();
        let commitment = rpc_client.commitment();
        let mut accounts = rpc_client
            .get_multiple_accounts_with_config(
                pubkeys,
                RpcAccountInfoConfig {
                    encoding: Some(UiAccountEncoding::Base64Zstd),
                    commitment: Some(commitment),
                    data_slice: None,
                    min_context_slot: Some(min_context_slot),
                },
                None,
            )
            .await
            .map_err(|err| {
                TaskInfoFetcherError::map_client_error(min_context_slot, err)
            })?;

        let accounts = pubkeys
            .iter()
            .enumerate()
            .map(|(i, pubkey)| {
                let account = if let Some(account) = accounts.get_mut(i) {
                    account
                } else {
                    return Err(TaskInfoFetcherError::AccountNotFoundError(
                        *pubkey,
                    ));
                };
                if let Some(account) = account.take() {
                    Ok(account)
                } else {
                    Err(TaskInfoFetcherError::AccountNotFoundError(*pubkey))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(accounts)
    }

    fn reserve_locks(&self, pubkeys: &[Pubkey]) -> CacheInnerGuard<'_> {
        // Sorted order is required: all callers acquire per-key locks in the same
        // order, preventing the A→B / B→A circular-wait deadlock.
        let mut pubkeys = pubkeys.to_vec();
        pubkeys.sort_unstable();
        pubkeys.dedup();

        let mut nonce_locks = vec![];
        {
            let mut inner = self.cache.lock().expect(MUTEX_POISONED_MSG);
            for pubkey in pubkeys {
                let (lock, eviceted) =
                    if let Some(val) = inner.active.get(&pubkey) {
                        (val.clone(), None)
                    } else if let Some(val) = inner.retiring.remove(&pubkey) {
                        // This promotes retiring to active
                        let evicted = inner.active.push(pubkey, val.clone());
                        (val, evicted)
                    } else {
                        let val = Arc::new(TMutex::new(u64::MAX));
                        let evicted = inner.active.push(pubkey, val.clone());
                        (val, evicted)
                    };

                if let Some((evicted_pk, evicted_lock)) = eviceted {
                    // If value isn't used by anyone then it can be dropped
                    if Arc::strong_count(&evicted_lock) > 1 {
                        // Value used in by another request
                        // We can't drop evicted lock in that case
                        // We move it to retiring, which will be cleaned up on exit
                        // Race condition scenario:
                        // 1. set of accs A evicted due to surge of requests - locks are dropped
                        // 2. request for set A still ongoing
                        // 3, another request with set A comes in, creating new locks in `CacheInner::active`
                        // 4. 2 simultaneous requestors receive same value
                        let old =
                            inner.retiring.insert(evicted_pk, evicted_lock);
                        if old.is_some() {
                            // Safety
                            // assume that is true:
                            // That means that value was active & retiring at the same time
                            // This is impossible as per logic above, contradiction. чтд.
                            debug_assert!(
                                false,
                                "Just eviceted value can't be in retiring"
                            );
                            error!("Retiring map already contained lock with pubkey: {}", evicted_pk);
                        }
                    }
                }

                nonce_locks.push((pubkey, lock));
            }
        }

        CacheInnerGuard {
            inner: &self.cache,
            nonce_locks,
        }
    }
}

/// TaskInfoFetcher implementation that also caches most used 1000 keys
#[async_trait]
impl TaskInfoFetcher for CacheTaskInfoFetcher {
    /// Returns next ids for requested pubkeys
    /// If key isn't in cache, it will be requested
    async fn fetch_next_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        if pubkeys.is_empty() {
            return Ok(HashMap::new());
        }

        let locks_guard = self.reserve_locks(pubkeys);

        // Acquire per-account locks sequentially in sorted order (see sort above).
        // join_all would poll all futures concurrently, allowing partial acquisition
        // and producing the classic A→B / B→A deadlock across concurrent callers.
        let guard_nonces = locks_guard.lock().await;
        let (mut existing, mut to_request) = (vec![], vec![]);
        for (pubkey, guard) in guard_nonces {
            if *guard == u64::MAX {
                to_request.push((pubkey, guard));
            } else {
                existing.push((pubkey, guard))
            }
        }

        // If all in cache - great! return
        if to_request.is_empty() {
            let mut result = HashMap::with_capacity(existing.len());
            // Consume guards & write result
            for (pubkey, mut guard) in existing {
                *guard += 1;
                result.insert(*pubkey, *guard);
            }

            return Ok(result);
        }

        // Remove duplicates
        let to_request_pubkeys: Vec<_> =
            to_request.iter().map(|(pubkey, _)| **pubkey).collect();
        let remaining_ids = Self::fetch_metadata_with_retries(
            &self.rpc_client,
            &to_request_pubkeys,
            min_context_slot,
            NUM_FETCH_RETRIES,
        )
        .await?
        .into_iter()
        .map(|metadata| metadata.last_update_nonce);

        // We don't care if anything changed in between with cache - just update and return our ids.
        let mut result = HashMap::with_capacity(existing.len());
        // Consume guards & write result
        for (pubkey, mut guard) in existing {
            *guard += 1;
            result.insert(*pubkey, *guard);
        }
        for ((pubkey, mut guard), nonce) in
            to_request.into_iter().zip(remaining_ids)
        {
            *guard = nonce + 1;
            result.insert(*pubkey, *guard);
        }

        Ok(result)
    }

    async fn fetch_current_commit_nonces(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        if pubkeys.is_empty() {
            return Ok(HashMap::new());
        }

        let locks_guard = self.reserve_locks(pubkeys);

        // Acquire per-account locks sequentially in sorted order (see sort above).
        let guard_nonces = locks_guard.lock().await;
        let mut to_request = vec![];
        let mut result = HashMap::with_capacity(guard_nonces.len());
        for (pubkey, guard) in guard_nonces {
            if *guard == u64::MAX {
                to_request.push((pubkey, guard));
            } else {
                result.insert(*pubkey, *guard);
            }
        }

        if to_request.is_empty() {
            return Ok(result);
        }

        let to_request_pubkeys: Vec<_> =
            to_request.iter().map(|(pubkey, _)| **pubkey).collect();
        let remaining_ids = Self::fetch_metadata_with_retries(
            &self.rpc_client,
            &to_request_pubkeys,
            min_context_slot,
            NUM_FETCH_RETRIES,
        )
        .await?
        .into_iter()
        .map(|metadata| metadata.last_update_nonce);

        // Store the on-chain nonce as-is (no +1): recording current state, not
        // reserving the next slot. A subsequent fetch_next_commit_nonces call will
        // increment from here correctly.
        for ((pubkey, mut guard), nonce) in
            to_request.into_iter().zip(remaining_ids)
        {
            *guard = nonce;
            result.insert(*pubkey, nonce);
        }

        Ok(result)
    }

    async fn fetch_rent_reimbursements(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
        let rent_reimbursements = Self::fetch_metadata_with_retries(
            &self.rpc_client,
            pubkeys,
            min_context_slot,
            NUM_FETCH_RETRIES,
        )
        .await?
        .into_iter()
        .map(|metadata| metadata.rent_payer)
        .collect();

        Ok(rent_reimbursements)
    }

    /// Returns current commit id without raising priority
    async fn peek_commit_nonce(&self, pubkey: &Pubkey) -> Option<u64> {
        let lock = {
            // Peek without promoting LRU order; also check retiring for in-flight keys.
            // Outer lock held only to clone the Arc, released before awaiting per-key lock.
            let inner = self.cache.lock().expect(MUTEX_POISONED_MSG);
            inner
                .active
                .peek(pubkey)
                .or_else(|| inner.retiring.get(pubkey))
                .cloned()
        }?;

        let locks_guard = CacheInnerGuard {
            inner: &self.cache,
            nonce_locks: vec![(*pubkey, lock)],
        };
        let guards = locks_guard.lock().await;
        let value = *guards[0].1;

        (value != u64::MAX).then_some(value)
    }

    /// Reset cache
    fn reset(&self, reset_type: ResetType) {
        let mut cache = self.cache.lock().expect(MUTEX_POISONED_MSG);
        match reset_type {
            ResetType::All => {
                cache.active.clear();
                cache.retiring.clear();
            }
            ResetType::Specific(pubkeys) => {
                for pubkey in pubkeys {
                    cache.active.pop(pubkey);
                    cache.retiring.remove(pubkey);
                }
            }
        }
    }

    async fn get_base_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>> {
        let accounts = Self::fetch_accounts_with_retries(
            &self.rpc_client,
            pubkeys,
            min_context_slot,
            NUM_FETCH_RETRIES,
        )
        .await?;

        Ok(pubkeys.iter().copied().zip(accounts).collect())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskInfoFetcherError {
    #[error("Metadata not found for: {0}")]
    AccountNotFoundError(Pubkey),
    #[error("InvalidAccountDataError for: {0}")]
    InvalidAccountDataError(Pubkey),
    #[error("Minimum context slot {0} not reached: {1}")]
    MinContextSlotNotReachedError(u64, Box<MagicBlockRpcClientError>),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(Box<MagicBlockRpcClientError>),
}

impl TaskInfoFetcherError {
    pub fn map_client_error(
        min_context_slot: u64,
        e: MagicBlockRpcClientError,
    ) -> Self {
        const MIN_CONTEXT_SLOT_MSG1: &str =
            "Minimum context slot has not been reached";

        let orig = e;
        let err = match &orig {
            MagicBlockRpcClientError::RpcClientError(err)
            | MagicBlockRpcClientError::SendTransaction(err) => Some(err),
            _ => None,
        };
        let Some(err) = err else {
            return Self::MagicBlockRpcClientError(Box::new(orig));
        };

        match &err.kind {
            ErrorKind::RpcError(rpc_err) => match rpc_err {
                RpcError::ForUser(msg)
                if msg.contains(MIN_CONTEXT_SLOT_MSG1) => {
                    Self::MinContextSlotNotReachedError(min_context_slot, Box::new(orig))
                },
                RpcError::RpcResponseError { code, .. }
                if *code == JSON_RPC_SERVER_ERROR_MIN_CONTEXT_SLOT_NOT_REACHED => {
                    Self::MinContextSlotNotReachedError(min_context_slot, Box::new(orig))
                }
                _ => Self::MagicBlockRpcClientError(Box::new(orig)),
            },
            _ => Self::MagicBlockRpcClientError(Box::new(orig)),
        }
    }
}

impl From<MagicBlockRpcClientError> for TaskInfoFetcherError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClientError(Box::new(e))
    }
}

impl TaskInfoFetcherError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::AccountNotFoundError(_) => None,
            Self::InvalidAccountDataError(_) => None,
            Self::MinContextSlotNotReachedError(_, err) => err.signature(),
            Self::MagicBlockRpcClientError(err) => err.signature(),
        }
    }
}

pub type TaskInfoFetcherResult<T, E = TaskInfoFetcherError> = Result<T, E>;

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use base64::{prelude::BASE64_STANDARD, Engine};
    use dlp::state::DelegationMetadata;
    use solana_account_decoder::{UiAccount, UiAccountData, UiAccountEncoding};
    use solana_pubkey::Pubkey;
    use solana_rpc_client::{
        nonblocking::rpc_client::RpcClient,
        rpc_client::RpcClientConfig,
        rpc_sender::{RpcSender, RpcTransportStats},
    };
    use solana_rpc_client_api::{
        client_error::Result as ClientResult,
        request::RpcRequest,
        response::{Response, RpcResponseContext},
    };

    use super::*;

    // ---- mock ----

    struct TestSender {
        calls: std::sync::Mutex<VecDeque<DelegationMetadata>>,
        rpc_delay: Option<Duration>,
    }

    impl TestSender {
        fn new(responses: Vec<DelegationMetadata>) -> Self {
            Self {
                calls: std::sync::Mutex::new(responses.into()),
                rpc_delay: None,
            }
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.rpc_delay = Some(delay);
            self
        }
    }

    #[async_trait]
    impl RpcSender for TestSender {
        async fn send(
            &self,
            request: RpcRequest,
            params: serde_json::Value,
        ) -> ClientResult<serde_json::Value> {
            if let Some(delay) = self.rpc_delay {
                tokio::time::sleep(delay).await;
            }
            assert_eq!(request, RpcRequest::GetMultipleAccounts);
            let n = params[0].as_array().map(|a| a.len()).unwrap_or(0);
            let metas: Vec<DelegationMetadata> = {
                let mut q = self.calls.lock().unwrap();
                (0..n)
                    .map(|_| {
                        q.pop_front()
                            .expect("unexpected RPC call: queue exhausted")
                    })
                    .collect()
            };
            let accounts: Vec<Option<UiAccount>> =
                metas.iter().map(|m| Some(encode_meta(m))).collect();
            Ok(serde_json::to_value(Response {
                context: RpcResponseContext {
                    slot: 1,
                    api_version: None,
                },
                value: accounts,
            })
            .unwrap())
        }

        fn get_transport_stats(&self) -> RpcTransportStats {
            RpcTransportStats::default()
        }

        fn url(&self) -> String {
            "test".to_string()
        }
    }

    fn encode_meta(m: &DelegationMetadata) -> UiAccount {
        let mut bytes = Vec::new();
        m.to_bytes_with_discriminator(&mut bytes).unwrap();
        UiAccount {
            lamports: 1_000_000,
            data: UiAccountData::Binary(
                BASE64_STANDARD.encode(&bytes),
                UiAccountEncoding::Base64,
            ),
            owner: dlp::id().to_string(),
            executable: false,
            rent_epoch: 0,
            space: Some(bytes.len() as u64),
        }
    }

    fn meta(nonce: u64) -> DelegationMetadata {
        DelegationMetadata {
            last_update_nonce: nonce,
            is_undelegatable: false,
            seeds: vec![],
            rent_payer: Pubkey::default(),
        }
    }

    struct FetcherBuilder {
        sender: TestSender,
        capacity: Option<NonZeroUsize>,
    }

    impl FetcherBuilder {
        fn new(responses: Vec<DelegationMetadata>) -> Self {
            Self {
                sender: TestSender::new(responses),
                capacity: None,
            }
        }

        fn rpc_delay(mut self, d: Duration) -> Self {
            self.sender = self.sender.with_delay(d);
            self
        }

        fn capacity(mut self, n: usize) -> Self {
            self.capacity = Some(n.try_into().unwrap());
            self
        }

        fn build(self) -> CacheTaskInfoFetcher {
            let rpc = MagicblockRpcClient::new(Arc::new(
                RpcClient::new_sender(self.sender, RpcClientConfig::default()),
            ));
            match self.capacity {
                Some(cap) => CacheTaskInfoFetcher::with_capacity(cap, rpc),
                None => CacheTaskInfoFetcher::new(rpc),
            }
        }
    }

    // ---- tests ----

    #[tokio::test]
    async fn cache_miss_then_hit() {
        let pk = Pubkey::new_unique();
        let fetcher = FetcherBuilder::new(vec![meta(10)]).build();

        let r1 = fetcher.fetch_next_commit_nonces(&[pk], 0).await.unwrap();
        assert_eq!(r1[&pk], 11);

        // Cache hit: no RPC (only 1 response queued), increments
        let r2 = fetcher.fetch_next_commit_nonces(&[pk], 0).await.unwrap();
        assert_eq!(r2[&pk], 12);
    }

    #[tokio::test]
    async fn partial_cache_hit() {
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        // prime pk1 (nonce 5), then mixed call fetches only cold pk2 (nonce 20)
        let fetcher = FetcherBuilder::new(vec![meta(5), meta(20)]).build();

        fetcher.fetch_next_commit_nonces(&[pk1], 0).await.unwrap(); // pk1 = 6
        let r = fetcher
            .fetch_next_commit_nonces(&[pk1, pk2], 0)
            .await
            .unwrap();
        assert_eq!(r[&pk1], 7); // cached, incremented
        assert_eq!(r[&pk2], 21); // fetched from chain
    }

    #[tokio::test]
    async fn lru_eviction_forces_refetch() {
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        // pk1 initial, pk2 evicts pk1, pk1 re-fetch after eviction
        let fetcher = FetcherBuilder::new(vec![meta(1), meta(2), meta(10)])
            .capacity(1)
            .build();

        fetcher.fetch_next_commit_nonces(&[pk1], 0).await.unwrap(); // pk1 = 2
        fetcher.fetch_next_commit_nonces(&[pk2], 0).await.unwrap(); // pk2 = 3, pk1 evicted
        let r = fetcher.fetch_next_commit_nonces(&[pk1], 0).await.unwrap();
        assert_eq!(r[&pk1], 11); // re-fetched (10 + 1)
    }

    // Phase 1: fetch phase1_keys one-by-one → barrier → outer verification →
    // barrier. Phase 2: fetch phase2_keys one-by-one for `iters` passes, then
    // fetch shared_b in chunks of 2.
    async fn run_worker(
        fetcher: Arc<CacheTaskInfoFetcher>,
        barrier: Arc<tokio::sync::Barrier>,
        phase1_keys: Vec<Pubkey>,
        phase2_keys: Vec<Pubkey>,
        shared_b: Vec<Pubkey>,
        iters: usize,
    ) {
        for pk in &phase1_keys {
            fetcher.fetch_next_commit_nonces(&[*pk], 0).await.unwrap();
        }
        barrier.wait().await; // signal phase 1 done
        barrier.wait().await; // wait for outer verification
        for _ in 0..iters {
            for pk in &phase2_keys {
                fetcher.fetch_next_commit_nonces(&[*pk], 0).await.unwrap();
            }
        }
        for chunk in shared_b.chunks(2) {
            fetcher.fetch_next_commit_nonces(chunk, 0).await.unwrap();
        }
    }

    // Three concurrent workers operating in two phases, separated by a barrier.
    #[tokio::test(flavor = "multi_thread")]
    async fn three_concurrent_workers_two_phase() {
        const ITERS: usize = 50;
        const NUM_WORKERS: usize = 3;
        const SHARED_A: usize = 10;
        const SHARED_B: usize = 40;
        const EXCLUSIVE: usize = 50;
        const CAPACITY: usize = 30;
        const PHASE2_KEYS: usize = EXCLUSIVE + SHARED_A; // 60

        let shared_a: Vec<Pubkey> =
            (0..SHARED_A).map(|_| Pubkey::new_unique()).collect();
        let shared_b: Vec<Pubkey> =
            (0..SHARED_B).map(|_| Pubkey::new_unique()).collect();
        let excl: [Vec<Pubkey>; NUM_WORKERS] = std::array::from_fn(|_| {
            (0..EXCLUSIVE).map(|_| Pubkey::new_unique()).collect()
        });
        let phase2_keys: [Vec<Pubkey>; NUM_WORKERS] =
            std::array::from_fn(|i| {
                excl[i].iter().chain(shared_a.iter()).cloned().collect()
            });

        // Flat queue: each RPC call pops exactly N entries (N cold keys).
        // Upper bounds:
        //   phase 1 loop:       SHARED_A × 1
        //   phase 2 excl+sa:    ITERS × PHASE2_KEYS × NUM_WORKERS × 1
        //   phase 2 shared_b:   (SHARED_B / chunk_size) × chunk_size × NUM_WORKERS
        //                     = SHARED_B × NUM_WORKERS
        let total = SHARED_A
            + ITERS * PHASE2_KEYS * NUM_WORKERS
            + SHARED_B * NUM_WORKERS;
        let responses: Vec<DelegationMetadata> =
            (0..total).map(|_| meta(0)).collect();

        let fetcher = Arc::new(
            FetcherBuilder::new(responses)
                .capacity(CAPACITY)
                .rpc_delay(Duration::from_millis(2))
                .build(),
        );

        // Barrier resets automatically: round 1 syncs after phase 1, round 2
        // releases workers into phase 2 after outer verification.
        let barrier = Arc::new(tokio::sync::Barrier::new(NUM_WORKERS + 1));

        let handles: Vec<_> = (0..NUM_WORKERS)
            .map(|i| {
                tokio::spawn(run_worker(
                    fetcher.clone(),
                    barrier.clone(),
                    shared_a.clone(),
                    phase2_keys[i].clone(),
                    shared_b.clone(),
                    ITERS,
                ))
            })
            .collect();

        barrier.wait().await; // all workers done with phase 1

        // No eviction during phase 1 (10 keys < capacity 30).
        // Per-key lock serialises 3 workers → exactly NUM_WORKERS increments each.
        for pk in &shared_a {
            assert_eq!(
                fetcher.peek_commit_nonce(pk).await,
                Some(NUM_WORKERS as u64)
            );
        }

        barrier.wait().await; // release workers into phase 2

        for h in handles {
            h.await.unwrap();
        }

        // Workers fetched shared_b in chunks of 2 (40 / 2 = 20 calls each).
        // Due to eviction (40 keys > capacity 30) not all may remain in cache.
        // Any key still in cache must have nonce >= 1.
        let mut found = 0usize;
        for pk in &shared_b {
            if let Some(n) = fetcher.peek_commit_nonce(pk).await {
                assert!(n >= 1);
                found += 1;
            }
        }
        assert!(
            found > 0,
            "expected some shared_b keys in cache after workers"
        );
    }

    #[tokio::test]
    async fn fetch_current_no_increment() {
        let pk = Pubkey::new_unique();
        let fetcher = FetcherBuilder::new(vec![meta(10)]).build();

        let r1 = fetcher.fetch_current_commit_nonces(&[pk], 0).await.unwrap();
        assert_eq!(r1[&pk], 10); // stored as-is

        // Cache hit: still 10, fetch_current never increments
        let r2 = fetcher.fetch_current_commit_nonces(&[pk], 0).await.unwrap();
        assert_eq!(r2[&pk], 10);
    }

    #[tokio::test]
    async fn reset_all_forces_refetch() {
        let pk = Pubkey::new_unique();
        let fetcher = FetcherBuilder::new(vec![meta(5), meta(99)]).build();

        fetcher.fetch_next_commit_nonces(&[pk], 0).await.unwrap(); // pk = 6
        fetcher.reset(ResetType::All);

        let r = fetcher.fetch_next_commit_nonces(&[pk], 0).await.unwrap();
        assert_eq!(r[&pk], 100); // re-fetched
    }

    #[tokio::test]
    async fn reset_specific_only_clears_that_key() {
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        // pk1 initial, pk2 initial, pk1 after reset
        let fetcher =
            FetcherBuilder::new(vec![meta(1), meta(2), meta(50)]).build();

        fetcher.fetch_next_commit_nonces(&[pk1], 0).await.unwrap(); // pk1 = 2
        fetcher.fetch_next_commit_nonces(&[pk2], 0).await.unwrap(); // pk2 = 3
        fetcher.reset(ResetType::Specific(&[pk1]));

        let r1 = fetcher.fetch_next_commit_nonces(&[pk1], 0).await.unwrap();
        let r2 = fetcher.fetch_next_commit_nonces(&[pk2], 0).await.unwrap();
        assert_eq!(r1[&pk1], 51); // re-fetched (50 + 1)
        assert_eq!(r2[&pk2], 4); // still cached (3 + 1)
    }

    #[tokio::test]
    async fn peek_does_not_increment() {
        let pk = Pubkey::new_unique();
        let fetcher = FetcherBuilder::new(vec![meta(7)]).build();

        assert_eq!(fetcher.peek_commit_nonce(&pk).await, None);

        fetcher.fetch_next_commit_nonces(&[pk], 0).await.unwrap(); // pk = 8
        assert_eq!(fetcher.peek_commit_nonce(&pk).await, Some(8));
        assert_eq!(fetcher.peek_commit_nonce(&pk).await, Some(8)); // unchanged

        let r = fetcher.fetch_next_commit_nonces(&[pk], 0).await.unwrap();
        assert_eq!(r[&pk], 9); // peek didn't change the value
    }
}
