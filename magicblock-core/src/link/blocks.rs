use solana_hash::Hash;
use tokio::sync::broadcast;

/// A type alias for the cryptographic hash of a block.
pub type BlockHash = Hash;

/// A receiver for block update notifications.
/// The notification payload is empty; consumers should load the latest block
/// from the ledger when notified.
pub type BlockUpdateRx<T> = broadcast::Receiver<T>;
