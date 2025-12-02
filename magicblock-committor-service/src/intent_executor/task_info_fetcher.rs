use std::{
    collections::HashMap, num::NonZeroUsize, sync::Mutex, time::Duration,
};

use async_trait::async_trait;
use dlp::{
    delegation_metadata_seeds_from_delegated_account, state::DelegationMetadata,
};
use log::{error, warn};
use lru::LruCache;
use magicblock_metrics::metrics;
use magicblock_rpc_client::{MagicBlockRpcClientError, MagicblockRpcClient};
use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;

const NUM_FETCH_RETRIES: NonZeroUsize =
    NonZeroUsize::new(5).unwrap();
const MUTEX_POISONED_MSG: &str = "CacheTaskInfoFetcher mutex poisoned!";

#[async_trait]
pub trait TaskInfoFetcher: Send + Sync + 'static {
    /// Fetches correct next ids for pubkeys
    /// Those ids can be used as correct commit_id during Commit
    async fn fetch_next_commit_ids(
        &self,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>>;

    /// Fetches rent reimbursement address for pubkeys
    async fn fetch_rent_reimbursements(
        &self,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<Vec<Pubkey>>;

    /// Peeks current commit ids for pubkeys
    fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<u64>;

    /// Resets cache for some or all accounts
    fn reset(&self, reset_type: ResetType);
}

pub enum ResetType<'a> {
    All,
    Specific(&'a [Pubkey]),
}

pub struct CacheTaskInfoFetcher {
    rpc_client: MagicblockRpcClient,
    cache: Mutex<LruCache<Pubkey, u64>>,
}

impl CacheTaskInfoFetcher {
    pub fn new(rpc_client: MagicblockRpcClient) -> Self {
        const CACHE_SIZE: NonZeroUsize =
            NonZeroUsize::new(1000).unwrap();

        Self {
            rpc_client,
            cache: Mutex::new(LruCache::new(CACHE_SIZE)),
        }
    }

    /// Fetches [`DelegationMetadata`]s with some num of retries
    pub async fn fetch_metadata_with_retries(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        num_retries: NonZeroUsize,
    ) -> TaskInfoFetcherResult<Vec<DelegationMetadata>> {
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        let mut last_err =
            TaskInfoFetcherError::MetadataNotFoundError(pubkeys[0]);
        for i in 0..num_retries.get() {
            match Self::fetch_metadata(rpc_client, pubkeys).await {
                Ok(value) => return Ok(value),
                err @ Err(TaskInfoFetcherError::InvalidAccountDataError(_)) => {
                    return err
                }
                err @ Err(TaskInfoFetcherError::MetadataNotFoundError(_)) => {
                    return err
                }
                Err(TaskInfoFetcherError::MagicBlockRpcClientError(err)) => {
                    // TODO(edwin): RPC error handlings should be more robust
                    last_err =
                        TaskInfoFetcherError::MagicBlockRpcClientError(err)
                }
            };

            warn!("Fetch commit last error: {}, attempt: {}", last_err, i);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Err(last_err)
    }

    /// Fetches commit_ids using RPC
    pub async fn fetch_metadata(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<Vec<DelegationMetadata>> {
        // Early return if no pubkeys to process
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        let pda_accounts = pubkeys
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
            .collect::<Vec<_>>();

        metrics::inc_task_info_fetcher_a_count();
        let accounts_data = rpc_client
            .get_multiple_accounts(&pda_accounts, None)
            .await?;

        let metadatas = pda_accounts
            .into_iter()
            .enumerate()
            .map(|(i, pda)| {
                let account = if let Some(account) = accounts_data.get(i) {
                    account
                } else {
                    return Err(TaskInfoFetcherError::MetadataNotFoundError(
                        pda,
                    ));
                };

                let account = account
                    .as_ref()
                    .ok_or(TaskInfoFetcherError::MetadataNotFoundError(pda))?;
                let metadata =
                    DelegationMetadata::try_from_bytes_with_discriminator(
                        &account.data,
                    )
                    .map_err(|_| {
                        TaskInfoFetcherError::InvalidAccountDataError(pda)
                    })?;

                Ok(metadata)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(metadatas)
    }
}

/// TaskInfoFetcher implementation that also caches most used 1000 keys
#[async_trait]
impl TaskInfoFetcher for CacheTaskInfoFetcher {
    /// Returns next ids for requested pubkeys
    /// If key isn't in cache, it will be requested
    async fn fetch_next_commit_ids(
        &self,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
        if pubkeys.is_empty() {
            return Ok(HashMap::new());
        }

        let mut result = HashMap::new();
        let mut to_request = Vec::new();
        // Lock cache and extract whatever ids we can
        {
            let mut cache = self.cache.lock().expect(MUTEX_POISONED_MSG);
            for pubkey in pubkeys {
                // in case already inserted
                if result.contains_key(pubkey) {
                    continue;
                }

                if let Some(id) = cache.get(pubkey) {
                    result.insert(*pubkey, *id + 1);
                } else {
                    to_request.push(*pubkey);
                }
            }
        }

        // If all in cache - great! return
        if to_request.is_empty() {
            let mut cache = self.cache.lock().expect(MUTEX_POISONED_MSG);
            result.iter().for_each(|(pubkey, id)| {
                cache.push(*pubkey, *id);
            });

            return Ok(result);
        }

        // Remove duplicates
        to_request.sort();
        to_request.dedup();

        let remaining_ids = Self::fetch_metadata_with_retries(
            &self.rpc_client,
            &to_request,
            NUM_FETCH_RETRIES,
        )
        .await?
        .into_iter()
        .map(|metadata| metadata.last_update_nonce);

        // We don't care if anything changed in between with cache - just update and return our ids.
        {
            let mut cache = self.cache.lock().expect(MUTEX_POISONED_MSG);
            // Avoid changes to LRU until all data is ready - atomic update
            result.iter().for_each(|(pubkey, id)| {
                cache.push(*pubkey, *id);
            });
            to_request
                .iter()
                .zip(remaining_ids)
                .for_each(|(pubkey, id)| {
                    result.insert(*pubkey, id + 1);
                    cache.push(*pubkey, id + 1);
                });
        }

        Ok(result)
    }

    async fn fetch_rent_reimbursements(
        &self,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
        let rent_reimbursements = Self::fetch_metadata_with_retries(
            &self.rpc_client,
            pubkeys,
            NUM_FETCH_RETRIES,
        )
        .await?
        .into_iter()
        .map(|metadata| metadata.rent_payer)
        .collect();

        Ok(rent_reimbursements)
    }

    /// Returns current commit id without raising priority
    fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<u64> {
        let cache = self.cache.lock().expect(MUTEX_POISONED_MSG);
        cache.peek(pubkey).copied()
    }

    /// Reset cache
    fn reset(&self, reset_type: ResetType) {
        match reset_type {
            ResetType::All => {
                self.cache.lock().expect(MUTEX_POISONED_MSG).clear()
            }
            ResetType::Specific(pubkeys) => {
                let mut cache = self.cache.lock().expect(MUTEX_POISONED_MSG);
                pubkeys.iter().for_each(|pubkey| {
                    let _ = cache.pop(pubkey);
                });
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskInfoFetcherError {
    #[error("Metadata not found for: {0}")]
    MetadataNotFoundError(Pubkey),
    #[error("InvalidAccountDataError for: {0}")]
    InvalidAccountDataError(Pubkey),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(Box<MagicBlockRpcClientError>),
}

impl From<MagicBlockRpcClientError> for TaskInfoFetcherError {
    fn from(e: MagicBlockRpcClientError) -> Self {
        Self::MagicBlockRpcClientError(Box::new(e))
    }
}

impl TaskInfoFetcherError {
    pub fn signature(&self) -> Option<Signature> {
        match self {
            Self::MetadataNotFoundError(_) => None,
            Self::InvalidAccountDataError(_) => None,
            Self::MagicBlockRpcClientError(err) => err.signature(),
        }
    }
}

pub type TaskInfoFetcherResult<T, E = TaskInfoFetcherError> = Result<T, E>;
