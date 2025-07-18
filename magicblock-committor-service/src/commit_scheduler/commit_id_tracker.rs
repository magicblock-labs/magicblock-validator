use std::{collections::HashMap, num::NonZeroUsize};

use lru::LruCache;
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockRpcClientResult, MagicblockRpcClient,
};
use solana_pubkey::Pubkey;

//
pub struct CommitIdTracker {
    rpc_client: MagicblockRpcClient,
    cache: LruCache<Pubkey, u64>,
}

impl CommitIdTracker {
    pub fn new(rpc_client: MagicblockRpcClient) -> Self {
        const CACHE_SIZE: NonZeroUsize =
            unsafe { NonZeroUsize::new_unchecked(1000) };

        Self {
            rpc_client,
            cache: LruCache::new(CACHE_SIZE),
        }
    }

    /// Returns next ids for requested pubkeys
    /// If key isn't in cache, it will be requested
    pub async fn next_commit_ids(
        &mut self,
        pubkeys: &[Pubkey],
    ) -> CommitIdTrackerResult<HashMap<Pubkey, u64>> {
        let mut result = HashMap::new();
        let mut to_request = Vec::new();
        for pubkey in pubkeys {
            // in case already inserted
            if result.contains_key(pubkey) {
                continue;
            }

            if let Some(id) = self.cache.get_mut(pubkey) {
                *id += 1;
                result.insert(*pubkey, *id);
            } else {
                to_request.push(*pubkey);
            }
        }

        // Remove duplicates
        to_request.sort();
        to_request.dedup();

        let remaining_ids =
            Self::fetch_commit_ids(&self.rpc_client, &to_request).await?;
        to_request
            .iter()
            .zip(remaining_ids)
            .for_each(|(pubkey, id)| {
                result.insert(*pubkey, id + 1);
                self.cache.push(*pubkey, id + 1);
            });

        Ok(result)
    }

    /// Returns current commit id without raising priority
    pub fn peek_commit_id(&self, pubkey: &Pubkey) -> Option<&u64> {
        self.cache.peek(pubkey)
    }

    /// Fetches commit_ids using RPC
    /// Note: remove duplicates prior to calling
    pub async fn fetch_commit_ids(
        rpc_client: &MagicblockRpcClient,
        pubkeys: &[Pubkey],
    ) -> MagicBlockRpcClientResult<Vec<u64>> {
        todo!()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Failed to get keys: {0:?}")]
    GetCommitIdsError(Vec<u64>),
    #[error("MagicBlockRpcClientError: {0}")]
    MagicBlockRpcClientError(#[from] MagicBlockRpcClientError),
}

pub type CommitIdTrackerResult<T, E = Error> = Result<T, E>;
