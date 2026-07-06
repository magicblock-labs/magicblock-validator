use std::sync::Arc;

pub(crate) mod subscriptions;

use engine::Engine;
use magicblock_chainlink::{ProdChainlink, ProdInnerChainlink};
use magicblock_ledger_deprecated::Ledger;
use solana_feature_set::FeatureSet;
use solana_pubkey::Pubkey;

pub type InnerChainlinkImpl = ProdInnerChainlink;

pub type ChainlinkImpl = ProdChainlink;

/// A container for the shared, global state of the RPC service.
///
/// All durable state (accounts, blocks, transactions, subscriptions) now lives
/// in the engine's keeper, so the RPC service only needs a handle to the engine
/// plus the read-only deprecated ledger it falls back to for historical reads.
#[derive(Clone)]
pub struct SharedState {
    /// The public key of the validator node and fee/feature context.
    pub(crate) context: NodeContext,
    /// The engine: single source of truth for account/block/transaction reads,
    /// subscriptions, and transaction submission.
    pub(crate) engine: Engine,
    /// Read-only handle to the deprecated ledger, used as a fallback for
    /// historical reads the engine's ledger no longer retains.
    pub(crate) ledger: Arc<Ledger>,
    /// Chainlink provides synchronization of on-chain accounts.
    pub(crate) chainlink: Arc<ChainlinkImpl>,
}

/// Holds the core configuration and runtime parameters that define the node's operational context.
#[derive(Default, Clone)]
pub struct NodeContext {
    /// The public key of the validator node.
    pub identity: Pubkey,
    /// Whether this node may accept or simulate locally submitted transactions.
    pub is_primary: bool,
    /// Base fee charged for transaction execution per signature.
    pub base_fee: u64,
    /// Runtime features activated for this node (used to compute fees)
    pub featureset: Arc<FeatureSet>,
    /// Block production time in milliseconds
    pub blocktime: u64,
}

impl SharedState {
    /// Initializes the shared state for the RPC service.
    pub fn new(
        context: NodeContext,
        engine: Engine,
        ledger: Arc<Ledger>,
        chainlink: Arc<ChainlinkImpl>,
    ) -> Self {
        Self {
            context,
            engine,
            ledger,
            chainlink,
        }
    }
}
