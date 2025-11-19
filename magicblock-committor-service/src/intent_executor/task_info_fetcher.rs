use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use borsh::BorshDeserialize;
use compressed_delegation_client::CompressedDelegationRecord;
use dlp::{
    delegation_metadata_seeds_from_delegated_account, state::DelegationMetadata,
};
use light_client::{
    indexer::{photon_indexer::PhotonIndexer, Indexer, IndexerError},
    rpc::RpcError,
};
use log::{error, warn};
use lru::LruCache;
use magicblock_core::compression::derive_cda_from_pda;
use magicblock_rpc_client::{MagicBlockRpcClientError, MagicblockRpcClient};
use solana_pubkey::Pubkey;

const NUM_FETCH_RETRIES: NonZeroUsize =
    unsafe { NonZeroUsize::new_unchecked(5) };
const MUTEX_POISONED_MSG: &str = "CacheTaskInfoFetcher mutex poisoned!";

#[async_trait]
pub trait TaskInfoFetcher: Send + Sync + 'static {
    /// Fetches correct next ids for pubkeys
    /// Those ids can be used as correct commit_id during Commit
    async fn fetch_next_commit_ids(
        &self,
        pubkeys: &[Pubkey],
        compressed: bool,
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
    photon_client: Arc<PhotonIndexer>,
    cache: Mutex<LruCache<Pubkey, u64>>,
}

impl CacheTaskInfoFetcher {
    pub fn new(
        rpc_client: MagicblockRpcClient,
        photon_client: Arc<PhotonIndexer>,
    ) -> Self {
        const CACHE_SIZE: NonZeroUsize =
            unsafe { NonZeroUsize::new_unchecked(1000) };

        Self {
            rpc_client,
            photon_client,
            cache: Mutex::new(LruCache::new(CACHE_SIZE)),
        }
    }

    /// Generic fetch with retries that takes a closure as argument for the fetch method
    pub async fn fetch_with_retries<'a, F, Fut, T>(
        pubkeys: &'a [Pubkey],
        num_retries: NonZeroUsize,
        mut fetch_fn: F,
    ) -> Result<T, TaskInfoFetcherError>
    where
        T: Default,
        F: FnMut(&'a [Pubkey]) -> Fut + 'a,
        Fut: std::future::Future<Output = Result<T, TaskInfoFetcherError>>,
    {
        if pubkeys.is_empty() {
            return Ok(Default::default());
        }
        let mut last_err =
            TaskInfoFetcherError::MetadataNotFoundError(pubkeys[0]);
        for i in 0..num_retries.get() {
            let result = fetch_fn(pubkeys).await;
            match result {
                Ok(value) => return Ok(value),
                err @ Err(TaskInfoFetcherError::InvalidAccountDataError(_))
                | err @ Err(TaskInfoFetcherError::MetadataNotFoundError(_))
                | err @ Err(TaskInfoFetcherError::DeserializeError(_)) => {
                    return err
                }
                Err(TaskInfoFetcherError::LightRpcError(err)) => {
                    // TODO(edwin0: RPC error handlings should be more robust
                    last_err = TaskInfoFetcherError::LightRpcError(err)
                }
                Err(TaskInfoFetcherError::IndexerError(err)) => {
                    // TODO(edwin0: RPC error handlings should be more robust
                    last_err = TaskInfoFetcherError::IndexerError(err)
                }
                Err(TaskInfoFetcherError::MagicBlockRpcClientError(err)) => {
                    // TODO(edwin): RPC error handlings should be more robust
                    last_err =
                        TaskInfoFetcherError::MagicBlockRpcClientError(err)
                }
                Err(TaskInfoFetcherError::NoCompressedData(err)) => {
                    // TODO(edwin0: RPC error handlings should be more robust
                    last_err = TaskInfoFetcherError::NoCompressedData(err)
                }
                Err(TaskInfoFetcherError::NoCompressedAccount(err)) => {
                    // TODO(edwin0: RPC error handlings should be more robust
                    last_err = TaskInfoFetcherError::NoCompressedAccount(err)
                }
            }
            warn!("Fetch commit last error: {}, attempt: {}", last_err, i);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(last_err)
    }

    pub async fn fetch_metadata_with_retries(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
        num_retries: NonZeroUsize,
    ) -> TaskInfoFetcherResult<Vec<DelegationMetadata>> {
        Self::fetch_with_retries(pubkeys, num_retries, move |pubkeys| {
            Self::fetch_metadata(rpc_client, pubkeys)
        })
        .await
    }

    pub async fn fetch_compressed_delegation_records_with_retries(
        photon_client: &PhotonIndexer,
        pubkeys: &[Pubkey],
        num_retries: NonZeroUsize,
    ) -> TaskInfoFetcherResult<Vec<CompressedDelegationRecord>> {
        Self::fetch_with_retries(pubkeys, num_retries, move |pubkeys| {
            Self::fetch_compressed_delegation_record(photon_client, pubkeys)
        })
        .await
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

    /// Fetches commit_ids using RPC
    pub async fn fetch_compressed_delegation_record(
        photon_client: &PhotonIndexer,
        pubkeys: &[Pubkey],
    ) -> TaskInfoFetcherResult<Vec<CompressedDelegationRecord>> {
        // Early return if no pubkeys to process
        if pubkeys.is_empty() {
            return Ok(Vec::new());
        }

        let cdas = pubkeys
            .iter()
            .map(|pubkey| derive_cda_from_pda(pubkey).to_bytes())
            .collect::<Vec<_>>();
        let compressed_accounts = photon_client
            .get_multiple_compressed_accounts(Some(cdas), None, None)
            .await
            .map_err(TaskInfoFetcherError::IndexerError)?
            .value;

        let compressed_delegation_records = compressed_accounts
            .items
            .into_iter()
            .zip(pubkeys.iter())
            .map(|(acc, pubkey)| {
                let delegation_record =
                    CompressedDelegationRecord::try_from_slice(
                        &acc.ok_or(TaskInfoFetcherError::NoCompressedAccount(
                            *pubkey,
                        ))?
                        .data
                        .ok_or(TaskInfoFetcherError::NoCompressedData(*pubkey))?
                        .data,
                    )
                    .map_err(TaskInfoFetcherError::DeserializeError)?;

                Ok::<_, TaskInfoFetcherError>(delegation_record)
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(compressed_delegation_records)
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
        compressed: bool,
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

        let remaining_ids = if compressed {
            Self::fetch_compressed_delegation_records_with_retries(
                &self.photon_client,
                &to_request,
                NUM_FETCH_RETRIES,
            )
            .await?
            .into_iter()
            .map(|metadata| metadata.last_update_nonce)
            .collect::<Vec<_>>()
        } else {
            Self::fetch_metadata_with_retries(
                &self.rpc_client,
                &to_request,
                NUM_FETCH_RETRIES,
            )
            .await?
            .into_iter()
            .map(|metadata| metadata.last_update_nonce)
            .collect::<Vec<_>>()
        };

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
    #[error("LightRpcError: {0}")]
    LightRpcError(#[from] RpcError),
    #[error("Metadata not found for: {0}")]
    MetadataNotFoundError(Pubkey),
    #[error("InvalidAccountDataError for: {0}")]
    InvalidAccountDataError(Pubkey),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
    #[error("IndexerError: {0}")]
    IndexerError(#[from] IndexerError),
    #[error("IndexerError: {0}")]
    NoCompressedAccount(Pubkey),
    #[error("CompressedAccountDataNotFound: {0}")]
    NoCompressedData(Pubkey),
    #[error("CompressedAccountDataNotFound: {0}")]
    DeserializeError(#[from] std::io::Error),
}

pub type TaskInfoFetcherResult<T, E = TaskInfoFetcherError> = Result<T, E>;
