use std::{ops::Deref, sync::Arc, time::Duration};

use arc_swap::ArcSwapAny;
use magicblock_core::{
    link::blocks::{BlockHash, BlockMeta, BlockUpdate},
    Slot,
};
use solana_rpc_client_api::response::RpcBlockhash;

use super::ExpiringCache;

/// The standard block time for the Solana network, in milliseconds.
const SOLANA_BLOCK_TIME: f64 = 400.0;
/// The number of slots for which a blockhash is considered valid on the Solana network.
const MAX_VALID_BLOCKHASH_SLOTS: f64 = 150.0;

/// A thread-safe cache for recent block information.
///
/// This structure serves two primary functions:
/// 1.  It stores the single **latest** block for quick access to the current block height and hash.
/// 2.  It maintains a time-limited **cache** of recent blockhashes to validate incoming transactions.
pub(crate) struct BlocksCache {
    /// The number of slots for which a blockhash is considered valid.
    /// This is calculated based on the host ER's block time relative to Solana's.
    block_validity: u64,
    /// Latest observed block (updated whenever the ledger transitions to new slot)
    latest: ArcSwapAny<Arc<LastCachedBlock>>,
    /// An underlying time-based cache for storing `BlockHash` to `BlockMeta` mappings.
    cache: ExpiringCache<BlockHash, BlockMeta>,
}

/// Last produced block that has been put into cache. We need to keep this separately,
/// as there's no way to access the cache efficiently to find the latest inserted entry
#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct LastCachedBlock {
    pub(crate) blockhash: BlockHash,
    pub(crate) slot: Slot,
}

impl Deref for BlocksCache {
    type Target = ExpiringCache<BlockHash, BlockMeta>;
    fn deref(&self) -> &Self::Target {
        &self.cache
    }
}

impl BlocksCache {
    /// Creates a new `BlocksCache`.
    ///
    /// The `blocktime` parameter is used to dynamically calculate the blockhash validity
    /// period, making the cache adaptable to ERss with different block production speeds.
    ///
    /// # Panics
    /// Panics if `blocktime` is zero.
    pub(crate) fn new(blocktime: u64, latest: LastCachedBlock) -> Self {
        const BLOCK_CACHE_TTL: Duration = Duration::from_secs(60);
        assert!(blocktime != 0, "blocktime cannot be zero");

        // Adjust blockhash validity based on the ratio of the current
        // ER's block time to the standard Solana block time.
        let blocktime_ratio = SOLANA_BLOCK_TIME / blocktime as f64;
        let block_validity = blocktime_ratio * MAX_VALID_BLOCKHASH_SLOTS;
        let cache = ExpiringCache::new(BLOCK_CACHE_TTL);
        Self {
            latest: ArcSwapAny::new(latest.into()),
            block_validity: block_validity as u64,
            cache,
        }
    }

    /// Updates the latest block information in the cache.
    pub(crate) fn set_latest(&self, latest: BlockUpdate) {
        let last = LastCachedBlock {
            blockhash: latest.hash,
            slot: latest.meta.slot,
        };

        // Register the block in the expiring cache
        self.cache.push(latest.hash, latest.meta);
        // And mark it as latest observed
        self.latest.swap(last.into());
    }

    /// Retrieves information about the latest block, including its calculated validity period.
    pub(crate) fn get_latest(&self) -> BlockHashInfo {
        let block = self.latest.load();
        BlockHashInfo {
            hash: block.blockhash,
            validity: block.slot + self.block_validity,
            slot: block.slot,
        }
    }

    /// Returns the slot number of the most recent block, also known as the block height.
    pub(crate) fn block_height(&self) -> Slot {
        self.latest.load().slot
    }
}

/// A data structure containing essential details about a blockhash for RPC responses.
#[derive(Default)]
pub(crate) struct BlockHashInfo {
    /// The blockhash.
    pub(crate) hash: BlockHash,
    /// The last slot number at which this blockhash is still considered valid.
    pub(crate) validity: Slot,
    /// The slot in which the block was produced.
    pub(crate) slot: Slot,
}

impl From<BlockHashInfo> for RpcBlockhash {
    fn from(value: BlockHashInfo) -> Self {
        Self {
            blockhash: value.hash.to_string(),
            last_valid_block_height: value.validity,
        }
    }
}
