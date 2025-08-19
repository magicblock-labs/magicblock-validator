use std::sync::Arc;

use magicblock_core::link::transactions::TransactionResult;
use solana_signature::Signature;

use crate::Slot;

use super::ExpiringCache;

pub type TransactionsCache = Arc<ExpiringCache<Signature, SignatureResult>>;

#[derive(Clone)]
pub(crate) struct SignatureResult {
    pub slot: Slot,
    pub result: TransactionResult,
}
