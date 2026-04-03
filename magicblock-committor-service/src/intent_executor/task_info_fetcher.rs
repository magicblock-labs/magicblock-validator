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
use futures_util::{stream::FuturesUnordered, TryStreamExt};
use light_client::{
    indexer::{
        photon_indexer::PhotonIndexer, Indexer, IndexerError, IndexerRpcConfig,
        RetryConfig,
    },
    rpc::RpcError as LightRpcError,
};
use light_sdk::instruction::{
    account_meta::CompressedAccountMeta, PackedAccounts,
    SystemAccountMetaConfig,
};
use lru::LruCache;
use magicblock_core::compression::derive_cda_from_pda;
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
use tracing::{error, info, warn};

use crate::tasks::task_builder::CompressedData;

const NUM_FETCH_RETRIES: NonZeroUsize = NonZeroUsize::new(5).unwrap();
const MUTEX_POISONED_MSG: &str = "CacheTaskInfoFetcher mutex poisoned!";

#[async_trait]
pub trait TaskInfoFetcher: Send + Sync + 'static {
    /// Fetches correct next ids for pubkeys
    /// Those ids can be used as correct commit_id during Commit
    async fn fetch_next_commit_ids(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        compressed: bool,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>>;

    /// Fetches rent reimbursement address for pubkeys
    async fn fetch_rent_reimbursements(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<Vec<Pubkey>>;

    /// Peeks current commit ids for pubkeys
    fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<u64>;

    /// Resets cache for some or all accounts
    fn reset(&self, reset_type: ResetType);

    async fn get_base_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> TaskInfoFetcherResult<HashMap<Pubkey, Account>>;

    async fn get_compressed_data(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<u64>,
    ) -> TaskInfoFetcherResult<CompressedData>;

    async fn get_compressed_data_for_accounts(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: Option<u64>,
    ) -> TaskInfoFetcherResult<Vec<Option<CompressedData>>> {
        pubkeys
            .iter()
            .map(|pubkey| async move {
                Ok(Some(
                    self.get_compressed_data(pubkey, min_context_slot).await?,
                ))
            })
            .collect::<FuturesUnordered<_>>()
            .try_collect::<Vec<_>>()
            .await
    }
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
        const CACHE_SIZE: NonZeroUsize = NonZeroUsize::new(1000).unwrap();

        Self {
            rpc_client,
            photon_client,
            cache: Mutex::new(LruCache::new(CACHE_SIZE)),
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
                TaskInfoFetcherError::IndexerError(ref err) => {
                    warn!("Fetch compressed delegation records error: {:?}, attempt: {}", err, i);
                }
                TaskInfoFetcherError::NoCompressedAccount(_) => break Err(err),
                TaskInfoFetcherError::NoCompressedData(_) => break Err(err),
                TaskInfoFetcherError::DeserializeError(ref err) => {
                    warn!("Deserialize compressed delegation record error: {:?}, attempt: {}", err, i);
                }
                TaskInfoFetcherError::LightRpcError(ref err) => {
                    warn!("Fetch account error: {:?}, attempt: {}", err, i);
                }
                TaskInfoFetcherError::PhotonClientNotFound => break Err(err),
                TaskInfoFetcherError::LightSdkError(ref err) => {
                    warn!("LightSdk error: {:?}, attempt: {}", err, i);
                }
                TaskInfoFetcherError::MissingStateTrees => break Err(err),
                TaskInfoFetcherError::MissingAddress => break Err(err),
                TaskInfoFetcherError::MissingCompressedData => break Err(err),
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

    /// Fetches delegation records using Photon Indexer
    /// Photon
    pub async fn fetch_compressed_delegation_records_with_retries(
        photon_client: &PhotonIndexer,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
        max_retries: NonZeroUsize,
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
            .get_multiple_compressed_accounts(
                Some(cdas),
                None,
                Some(IndexerRpcConfig {
                    slot: min_context_slot,
                    retry_config: RetryConfig {
                        num_retries: max_retries.get() as u32,
                        ..Default::default()
                    },
                }),
            )
            .await
            .map_err(TaskInfoFetcherError::IndexerError)?
            .value;

        metrics::inc_task_info_fetcher_compressed_count();

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
        min_context_slot: u64,
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
                min_context_slot,
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
                min_context_slot,
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

    async fn get_compressed_data(
        &self,
        pubkey: &Pubkey,
        min_context_slot: Option<u64>,
    ) -> TaskInfoFetcherResult<CompressedData> {
        let cda = derive_cda_from_pda(pubkey);
        let compressed_delegation_record = self
            .photon_client
            .get_compressed_account(
                cda.to_bytes(),
                min_context_slot.map(|slot| IndexerRpcConfig {
                    slot,
                    retry_config: RetryConfig::default(),
                }),
            )
            .await
            .map_err(TaskInfoFetcherError::IndexerError)?
            .value;
        let proof_result = self
            .photon_client
            .get_validity_proof(
                vec![compressed_delegation_record.hash],
                vec![],
                min_context_slot.map(|slot| IndexerRpcConfig {
                    slot,
                    retry_config: RetryConfig::default(),
                }),
            )
            .await
            .map_err(TaskInfoFetcherError::IndexerError)?
            .value;

        let system_account_meta_config =
            SystemAccountMetaConfig::new(compressed_delegation_client::ID);
        let mut remaining_accounts = PackedAccounts::default();
        remaining_accounts
            .add_system_accounts_v2(system_account_meta_config)
            .map_err(TaskInfoFetcherError::LightSdkError)?;
        let packed_tree_accounts = proof_result
            .pack_tree_infos(&mut remaining_accounts)
            .state_trees
            .ok_or(TaskInfoFetcherError::MissingStateTrees)?;

        let tree_info = packed_tree_accounts
            .packed_tree_infos
            .first()
            .copied()
            .ok_or(TaskInfoFetcherError::MissingStateTrees)?;

        let account_meta = CompressedAccountMeta {
            tree_info,
            address: compressed_delegation_record
                .address
                .ok_or(TaskInfoFetcherError::MissingAddress)?,
            output_state_tree_index: packed_tree_accounts.output_tree_index,
        };

        Ok(CompressedData {
            hash: compressed_delegation_record.hash,
            compressed_delegation_record_bytes: compressed_delegation_record
                .data
                .ok_or(TaskInfoFetcherError::MissingCompressedData)?
                .data,
            remaining_accounts: remaining_accounts.to_account_metas().0,
            account_meta,
            proof: proof_result.proof,
        })
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TaskInfoFetcherError {
    #[error("LightRpcError: {0}")]
    LightRpcError(#[from] LightRpcError),
    #[error("Metadata not found for: {0}")]
    AccountNotFoundError(Pubkey),
    #[error("InvalidAccountDataError for: {0}")]
    InvalidAccountDataError(Pubkey),
    #[error("Minimum context slot {0} not reached: {1}")]
    MinContextSlotNotReachedError(u64, Box<MagicBlockRpcClientError>),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(Box<MagicBlockRpcClientError>),
    #[error("IndexerError: {0}")]
    IndexerError(#[from] IndexerError),
    #[error("NoCompressedAccount: {0}")]
    NoCompressedAccount(Pubkey),
    #[error("CompressedAccountDataNotFound: {0}")]
    NoCompressedData(Pubkey),
    #[error("CompressedAccountDataDeserializeError: {0}")]
    DeserializeError(#[from] std::io::Error),
    #[error("PhotonClientNotFound")]
    PhotonClientNotFound,
    #[error("LightSdkError: {0}")]
    LightSdkError(#[from] light_sdk::error::LightSdkError),
    #[error("MissingStateTrees")]
    MissingStateTrees,
    #[error("MissingAddress")]
    MissingAddress,
    #[error("MissingCompressedData")]
    MissingCompressedData,
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
            Self::IndexerError(_) => None,
            Self::NoCompressedAccount(_) => None,
            Self::NoCompressedData(_) => None,
            Self::DeserializeError(_) => None,
            Self::LightRpcError(_) => None,
            Self::PhotonClientNotFound => None,
            Self::LightSdkError(_) => None,
            Self::MissingStateTrees => None,
            Self::MissingAddress => None,
            Self::MissingCompressedData => None,
        }
    }
}

pub type TaskInfoFetcherResult<T, E = TaskInfoFetcherError> = Result<T, E>;
