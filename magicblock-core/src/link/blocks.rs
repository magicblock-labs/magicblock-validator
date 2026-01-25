#[cfg(not(feature = "tui"))]
use flume::{Receiver as MpmcReceiver, Sender as MpmcSender};
use solana_hash::Hash;
#[cfg(feature = "tui")]
use tokio::sync::broadcast::{
    Receiver as BroadcastReceiver, Sender as BroadcastSender,
};

use crate::Slot;

/// A type alias for the cryptographic hash of a block.
pub type BlockHash = Hash;
/// The receiving end of the channel for new block notifications.
#[cfg(feature = "tui")]
pub type BlockUpdateRx = BroadcastReceiver<BlockUpdate>;
#[cfg(not(feature = "tui"))]
pub type BlockUpdateRx = MpmcReceiver<BlockUpdate>;
/// The sending end of the channel for new block notifications.
#[cfg(feature = "tui")]
pub type BlockUpdateTx = BroadcastSender<BlockUpdate>;
#[cfg(not(feature = "tui"))]
pub type BlockUpdateTx = MpmcSender<BlockUpdate>;

/// Indicates why a receive did not yield a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvErrorKind {
    Empty,
    Lagged,
    Closed,
}

/// Creates a new receiver for block updates.
#[inline]
pub fn resubscribe_block_rx(rx: &BlockUpdateRx) -> BlockUpdateRx {
    #[cfg(feature = "tui")]
    {
        rx.resubscribe()
    }
    #[cfg(not(feature = "tui"))]
    {
        rx.clone()
    }
}

/// Receives the next block update, awaiting if needed.
pub async fn recv_block_update(
    rx: &mut BlockUpdateRx,
) -> Result<BlockUpdate, TryRecvErrorKind> {
    #[cfg(feature = "tui")]
    {
        rx.recv().await.map_err(|err| match err {
            tokio::sync::broadcast::error::RecvError::Lagged(_) => {
                TryRecvErrorKind::Lagged
            }
            tokio::sync::broadcast::error::RecvError::Closed => {
                TryRecvErrorKind::Closed
            }
        })
    }
    #[cfg(not(feature = "tui"))]
    {
        rx.recv_async().await.map_err(|_| TryRecvErrorKind::Closed)
    }
}

/// A type alias for a block's production timestamp, a Unix timestamp.
pub type BlockTime = i64;

/// A message representing a new block produced by the validator.
///
/// This is the primary message type sent over the block update channel to notify
/// listeners of new blocks.
#[derive(Default)]
#[cfg_attr(feature = "tui", derive(Clone))]
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
