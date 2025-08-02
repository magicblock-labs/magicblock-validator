use std::ops::Deref;

use magicblock_gateway_types::blocks::{BlockHash, BlockMeta, BlockUpdate};
use parking_lot::RwLock;

use crate::Slot;

use super::ExpiringCache;

const SOLANA_BLOCK_TIME: u64 = 400;
const MAX_VALID_BLOCKHASH_DURATION: u64 = 150;

pub(crate) struct BlocksCache {
    block_validity: u64,
    latest: RwLock<BlockUpdate>,
    cache: ExpiringCache<BlockHash, BlockMeta>,
}

impl Deref for BlocksCache {
    type Target = ExpiringCache<BlockHash, BlockMeta>;
    fn deref(&self) -> &Self::Target {
        &self.cache
    }
}

impl BlocksCache {
    pub(crate) fn new(blocktime: u64) -> Self {
        assert!(blocktime != 0, "blocktime cannot be zero");

        let block_validity = ((SOLANA_BLOCK_TIME as f64 / blocktime as f64)
            * MAX_VALID_BLOCKHASH_DURATION as f64)
            as u64;
        let cache = ExpiringCache::new(block_validity as usize);
        Self {
            latest: Default::default(),
            block_validity,
            cache,
        }
    }

    pub(crate) fn set_latest(&self, latest: BlockUpdate) {
        *self.latest.write() = latest;
    }

    pub(crate) fn get_latest(&self) -> BlockHashInfo {
        let guard = self.latest.read();
        BlockHashInfo {
            hash: guard.hash,
            validity: guard.meta.slot + self.block_validity,
            slot: guard.meta.slot,
        }
    }

    pub(crate) fn block_height(&self) -> Slot {
        self.latest.read().meta.slot
    }
}

pub(crate) struct BlockHashInfo {
    pub(crate) hash: BlockHash,
    pub(crate) validity: Slot,
    pub(crate) slot: Slot,
}
