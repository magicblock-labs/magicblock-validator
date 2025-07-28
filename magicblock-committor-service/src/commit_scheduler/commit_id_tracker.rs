use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use dlp::{
    delegation_metadata_seeds_from_delegated_account, state::DelegationMetadata,
};
use log::{error, warn};
use lru::LruCache;
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockRpcClientResult, MagicblockRpcClient,
};
use solana_pubkey::Pubkey;

#[async_trait::async_trait]
pub trait CommitIdTracker {
    async fn next_commit_ids(
        &mut self,
        pubkeys: &[Pubkey],
    ) -> CommitIdTrackerResult<HashMap<Pubkey, u64>>;

    fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<u64>;
}

const MUTEX_POISONED_MSG: &str = "CommitIdTrackerImpl mutex poisoned!";

#[derive(Clone)]
pub struct CommitIdTrackerImpl {
    rpc_client: MagicblockRpcClient,
    cache: Arc<Mutex<LruCache<Pubkey, u64>>>,
}

impl CommitIdTrackerImpl {
    pub fn new(rpc_client: MagicblockRpcClient) -> Self {
        const CACHE_SIZE: NonZeroUsize =
            unsafe { NonZeroUsize::new_unchecked(1000) };

        Self {
            rpc_client,
            cache: Arc::new(Mutex::new(LruCache::new(CACHE_SIZE))),
        }
    }

    /// Fetches commit_ids with some num of retries
    pub async fn fetch_commit_ids_with_retries(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        num_retries: NonZeroUsize,
    ) -> CommitIdTrackerResult<Vec<u64>> {
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        let mut last_err = Error::MetadataNotFoundError(pubkeys[0]);
        for i in 0..num_retries.get() {
            match Self::fetch_commit_ids(rpc_client, pubkeys).await {
                Ok(value) => return Ok(value),
                err @ Err(Error::InvalidAccountDataError(_)) => return err,
                err @ Err(Error::MetadataNotFoundError(_)) => return err,
                Err(Error::MagicBlockRpcClientError(err)) => {
                    // TODO: RPC error handlings should be more robust
                    last_err = Error::MagicBlockRpcClientError(err)
                }
            };

            warn!("Fetch commit last error: {}, attempt: {}", last_err, i);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Err(last_err)
    }

    /// Fetches commit_ids using RPC
    /// Note: remove duplicates prior to calling
    pub async fn fetch_commit_ids(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
    ) -> CommitIdTrackerResult<Vec<u64>> {
        // Early return if no pubkeys to process
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        // Find PDA accounts for each pubkey
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

        // Fetch account data for all PDAs
        let accounts_data = rpc_client
            .get_multiple_accounts(&pda_accounts, None)
            .await?;

        // Process each account data to extract last_update_external_slot
        let commit_ids = accounts_data
            .into_iter()
            .enumerate()
            .map(|(i, account)| {
                let pubkey = if let Some(pubkey) = pda_accounts.get(i) {
                    *pubkey
                } else {
                    error!("invalid pubkey index in pda_accounts: {i}");
                    Pubkey::new_unique()
                };
                let account = account
                    .ok_or(Error::MetadataNotFoundError(pda_accounts[i]))?;
                let metadata =
                    DelegationMetadata::try_from_bytes_with_discriminator(
                        &account.data,
                    )
                    .map_err(|_| Error::InvalidAccountDataError(pubkey))?;

                Ok::<_, Error>(metadata.last_update_external_slot)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(commit_ids)
    }
}

#[async_trait::async_trait]
impl CommitIdTracker for CommitIdTrackerImpl {
    /// Returns next ids for requested pubkeys
    /// If key isn't in cache, it will be requested
    async fn next_commit_ids(
        &mut self,
        pubkeys: &[Pubkey],
    ) -> CommitIdTrackerResult<HashMap<Pubkey, u64>> {
        const NUM_FETCH_RETRIES: NonZeroUsize =
            unsafe { NonZeroUsize::new_unchecked(5) };

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
            return Ok(result);
        }

        // Remove duplicates
        to_request.sort();
        to_request.dedup();

        let remaining_ids = Self::fetch_commit_ids_with_retries(
            &self.rpc_client,
            &to_request,
            NUM_FETCH_RETRIES,
        )
        .await?;

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

    /// Returns current commit id without raising priority
    fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<u64> {
        let cache = self.cache.lock().expect(MUTEX_POISONED_MSG);
        cache.peek(pubkey).copied()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Metadata not found for: {0}")]
    MetadataNotFoundError(Pubkey),
    #[error("InvalidAccountDataError for: {0}")]
    InvalidAccountDataError(Pubkey),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
}

pub type CommitIdTrackerResult<T, E = Error> = Result<T, E>;
