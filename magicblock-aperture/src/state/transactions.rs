use std::sync::Arc;

use magicblock_core::{link::transactions::TransactionResult, Slot};
use solana_signature::Signature;

use super::ExpiringCache;

/// A thread-safe, expiring cache for transaction signatures and their processing results.
///
/// It maps a `Signature` to an `Option<SignatureResult>`, allowing the cache to track a
/// signature even before its result is confirmed (by storing `None`).
pub type TransactionsCache =
    Arc<ExpiringCache<Signature, Option<SignatureResult>>>;

/// A compact representation of a transaction's processing outcome.
#[derive(Clone)]
pub(crate) struct SignatureResult {
    /// The slot in which the transaction was processed.
    pub slot: Slot,
    /// The result of the transaction (e.g., success or an error).
    pub result: TransactionResult,
}
