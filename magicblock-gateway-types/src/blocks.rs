use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
pub use solana_hash::Hash as BlockHash;

use crate::Slot;

/// Receiving end of block updates channel
pub type BlockUpdateRx = MpmcReceiver<BlockUpdate>;
/// Sending end of block updates channel
pub type BlockUpdateTx = MpmcSender<BlockUpdate>;

pub type BlockTime = i64;

#[derive(Default)]
pub struct BlockUpdate {
    pub meta: BlockMeta,
    pub hash: BlockHash,
}

#[derive(Default, Clone, Copy)]
pub struct BlockMeta {
    pub slot: Slot,
    pub time: BlockTime,
}
