use std::sync::Arc;

pub(crate) mod subscriptions;

use engine::Engine;
use magicblock_chainlink::ProdChainlink;
use magicblock_ledger_deprecated::Ledger;

/// A container for the shared, global state of the RPC service.
///
/// All durable state (accounts, blocks, transactions, subscriptions) now lives
/// in the engine's keeper, so the RPC service only needs a handle to the engine
/// plus the read-only deprecated ledger it falls back to for historical reads.
#[derive(Clone)]
pub struct SharedState {
    pub(crate) blocktime_ms: u64,
    /// The engine: single source of truth for account/block/transaction reads,
    /// subscriptions, and transaction submission.
    pub(crate) engine: Engine,
    /// Read-only handle to the deprecated ledger, used as a fallback for
    /// historical reads the engine's ledger no longer retains.
    pub(crate) ledger: Arc<Ledger>,
    /// Chainlink provides synchronization of on-chain accounts.
    pub(crate) chainlink: Arc<ProdChainlink>,
}

impl SharedState {
    pub fn new(
        engine: Engine,
        ledger: Arc<Ledger>,
        chainlink: Arc<ProdChainlink>,
        blocktime_ms: u64,
    ) -> Self {
        Self {
            blocktime_ms,
            engine,
            ledger,
            chainlink,
        }
    }
}
