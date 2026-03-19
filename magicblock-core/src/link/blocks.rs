use solana_hash::Hash;
use tokio::sync::broadcast;

/// A type alias for the cryptographic hash of a block.
pub type BlockHash = Hash;

/// A receiver for block update notifications.
/// Typically instantiated as `BlockUpdateRx<LatestBlockInner>` where the payload
/// contains the latest block data (slot, blockhash, timestamp).
pub type BlockUpdateRx<T> = broadcast::Receiver<T>;
