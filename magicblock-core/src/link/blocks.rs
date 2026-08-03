use solana_clock::Clock;
use solana_hash::Hash;
use tokio::sync::broadcast;

/// A type alias for the cryptographic hash of a block.
pub type BlockHash = Hash;

/// A receiver for block update notifications.
/// Typically instantiated as `BlockUpdateRx<LatestBlockInner>` where the payload
/// contains the latest block data (slot, blockhash, timestamp).
pub type BlockUpdateRx<T> = broadcast::Receiver<T>;

#[derive(Default, Clone)]
pub struct LatestBlockInner {
    pub slot: u64,
    pub blockhash: BlockHash,
    pub clock: Clock,
}

impl LatestBlockInner {
    pub fn new(slot: u64, blockhash: BlockHash, timestamp: i64) -> Self {
        let clock = Clock {
            slot: slot + 1,
            unix_timestamp: timestamp,
            ..Default::default()
        };

        Self {
            slot,
            blockhash,
            clock,
        }
    }
}
