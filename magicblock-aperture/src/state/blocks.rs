use std::{ops::Deref, time::Duration};

use magicblock_core::{
    link::blocks::{BlockHash, BlockMeta, BlockUpdate},
    Slot,
};
use magicblock_ledger::LatestBlock;
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
    /// The most recent block update received, protected by a `RwLock` for concurrent access.
    latest: LatestBlock,
    /// An underlying time-based cache for storing `BlockHash` to `BlockMeta` mappings.
    cache: ExpiringCache<BlockHash, BlockMeta>,
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
    pub(crate) fn new(blocktime: u64, latest: LatestBlock) -> Self {
        const BLOCK_CACHE_TTL: Duration = Duration::from_secs(60);
        assert!(blocktime != 0, "blocktime cannot be zero");

        // Adjust blockhash validity based on the ratio of the current
        // ER's block time to the standard Solana block time.
        let blocktime_ratio = SOLANA_BLOCK_TIME / blocktime as f64;
        let block_validity = blocktime_ratio * MAX_VALID_BLOCKHASH_SLOTS;
        let cache = ExpiringCache::new(BLOCK_CACHE_TTL);
        Self {
            latest,
            block_validity: block_validity as u64,
            cache,
        }
    }

    /// Updates the latest block information in the cache.
    pub(crate) fn set_latest(&self, latest: BlockUpdate) {
        // The `push` method adds the blockhash to the underlying expiring cache.
        self.cache.push(latest.hash, latest.meta);
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
