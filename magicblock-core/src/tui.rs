use solana_signature::Signature;
use tokio::sync::broadcast;

use crate::{link::blocks::BlockMeta, Slot};

pub const TUI_CHANNEL_CAPACITY: usize = 16_384;

/// Minimal block update payload for the TUI.
pub type TuiBlockUpdate = BlockMeta;
pub type TuiBlockUpdateTx = broadcast::Sender<TuiBlockUpdate>;
pub type TuiBlockUpdateRx = broadcast::Receiver<TuiBlockUpdate>;

/// Minimal transaction status payload for the TUI.
#[derive(Clone)]
pub struct TuiTransactionStatus {
    pub signature: Signature,
    pub slot: Slot,
    pub success: bool,
}

pub type TuiTransactionStatusTx = broadcast::Sender<TuiTransactionStatus>;
pub type TuiTransactionStatusRx = broadcast::Receiver<TuiTransactionStatus>;

pub fn tui_block_channel() -> (TuiBlockUpdateTx, TuiBlockUpdateRx) {
    broadcast::channel(TUI_CHANNEL_CAPACITY)
}

pub fn tui_transaction_status_channel(
) -> (TuiTransactionStatusTx, TuiTransactionStatusRx) {
    broadcast::channel(TUI_CHANNEL_CAPACITY)
}
