use std::{sync::Arc, time::Duration};

use blocks::BlocksCache;
use cache::ExpiringCache;
use magicblock_account_cloner::ChainlinkCloner;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::{
    remote_account_provider::{
        chain_pubsub_client::ChainPubsubClientImpl,
        chain_rpc_client::ChainRpcClientImpl,
    },
    submux::SubMuxClient,
    Chainlink,
};
use magicblock_ledger::Ledger;
use solana_feature_set::FeatureSet;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use subscriptions::SubscriptionsDb;
use transactions::TransactionsCache;

pub type ChainlinkImpl = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainPubsubClientImpl>,
    AccountsDb,
    ChainlinkCloner,
>;

/// A container for the shared, global state of the RPC service.
///
/// This struct aggregates thread-safe handles (`Arc`) and concurrently accessible
/// components (caches, databases) that need to be available across various parts
/// of the application, such as RPC handlers and event processors.
pub struct SharedState {
    /// The public key of the validator node.
    pub(crate) context: NodeContext,
    /// A thread-safe handle to the accounts database, which stores account states.
    pub(crate) accountsdb: Arc<AccountsDb>,
    /// A thread-safe handle to the blockchain ledger for accessing historical data.
    pub(crate) ledger: Arc<Ledger>,
    /// Chainlink provides synchronization of on-chain accounts
    pub(crate) chainlink: Arc<ChainlinkImpl>,
    /// A cache for recently processed transaction signatures to prevent replay attacks
    /// and to serve `getSignatureStatuses` requests efficiently.
    pub(crate) transactions: TransactionsCache,
    /// A cache for recent blockhashes, used for transaction validation and to serve
    /// block-related RPC requests.
    pub(crate) blocks: Arc<BlocksCache>,
    /// The central manager for all active pub-sub (e.g., WebSocket) subscriptions.
    pub(crate) subscriptions: SubscriptionsDb,
}

/// Holds the core configuration and runtime parameters that define the node's operational context.
#[derive(Default)]
pub struct NodeContext {
    /// The public key of the validator node.
    pub identity: Pubkey,
    /// The keypair for the optional faucet, used to airdrop tokens.
    pub faucet: Option<Keypair>,
    /// Base fee charged for transaction execution per signature.
    pub base_fee: u64,
    /// Runtime features activated for this node (used to compute fees)
    pub featureset: Arc<FeatureSet>,
}

impl SharedState {
    /// Initializes the shared state for the RPC service.
    ///
    /// # Security Note on TTLs
    ///
    /// The `TRANSACTIONS_CACHE_TTL` (75s) is intentionally set to be longer than the
    /// blockhash validity window (60s). This is a security measure to prevent a
    /// timing attack where a transaction's signature might be evicted from the cache
    /// before its blockhash expires, potentially allowing the transaction to be
    /// processed a second time.
    pub fn new(
        context: NodeContext,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        chainlink: Arc<ChainlinkImpl>,
        blocktime: u64,
    ) -> Self {
        const TRANSACTIONS_CACHE_TTL: Duration = Duration::from_secs(75);
        let latest = ledger.latest_block().clone();
        Self {
            context,
            accountsdb,
            transactions: ExpiringCache::new(TRANSACTIONS_CACHE_TTL).into(),
            blocks: BlocksCache::new(blocktime, latest).into(),
            ledger,
            chainlink,
            subscriptions: Default::default(),
        }
    }
}

pub(crate) mod blocks;
pub(crate) mod cache;
pub(crate) mod signatures;
pub(crate) mod subscriptions;
pub(crate) mod transactions;
