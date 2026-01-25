use solana_hash::Hash;
use tokio::sync::broadcast::{
    Receiver as BroadcastReceiver, Sender as BroadcastSender,
};

use crate::Slot;

/// A type alias for the cryptographic hash of a block.
pub type BlockHash = Hash;
/// The receiving end of the channel for new block notifications.
pub type BlockUpdateRx = BroadcastReceiver<BlockUpdate>;
/// The sending end of the channel for new block notifications.
pub type BlockUpdateTx = BroadcastSender<BlockUpdate>;

/// A type alias for a block's production timestamp, a Unix timestamp.
pub type BlockTime = i64;

/// A message representing a new block produced by the validator.
///
/// This is the primary message type sent over the block update channel to notify
/// listeners of new blocks.
#[derive(Default, Clone)]
pub struct BlockUpdate {
    /// The metadata associated with the block.
    pub meta: BlockMeta,
    /// The unique hash of the block.
    pub hash: BlockHash,
}

/// A collection of metadata associated with a block.
#[derive(Default, Clone, Copy)]
pub struct BlockMeta {
    /// The slot number in which the block was produced.
    pub slot: Slot,
    /// The timestamp of the block's production.
    pub time: BlockTime,
}
