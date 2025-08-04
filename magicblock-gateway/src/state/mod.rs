use std::sync::Arc;

use blocks::BlocksCache;
use cache::ExpiringCache;
use magicblock_accounts_db::AccountsDb;
use magicblock_ledger::Ledger;
use solana_pubkey::Pubkey;
use subscriptions::SubscriptionsDb;
use transactions::TransactionsCache;

#[derive(Clone)]
pub struct SharedState {
    pub(crate) identity: Pubkey,
    pub(crate) accountsdb: Arc<AccountsDb>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) transactions: TransactionsCache,
    pub(crate) blocks: Arc<BlocksCache>,
    pub(crate) subscriptions: SubscriptionsDb,
}

impl SharedState {
    pub fn new(
        identity: Pubkey,
        accountsdb: Arc<AccountsDb>,
        ledger: Arc<Ledger>,
        blocktime: u64,
    ) -> Self {
        Self {
            identity,
            accountsdb,
            ledger,
            transactions: ExpiringCache::new(16384 * 256).into(),
            blocks: BlocksCache::new(blocktime).into(),
            subscriptions: Default::default(),
        }
    }
}

pub(crate) mod blocks;
pub(crate) mod cache;
pub(crate) mod signatures;
pub(crate) mod subscriptions;
pub(crate) mod transactions;
