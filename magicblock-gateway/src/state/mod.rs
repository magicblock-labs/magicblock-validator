use std::sync::Arc;

use magicblock_accounts_db::AccountsDb;
use magicblock_ledger::Ledger;
use transactions_cache::TransactionsCache;

#[derive(Clone)]
pub struct SharedState {
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    txn_cache: Arc<TransactionsCache>,
}

impl SharedState {
    fn new(accountsdb: Arc<AccountsDb>, ledger: Arc<Ledger>) -> Self {
        let txn_cache = TransactionsCache::init();
        Self {
            accountsdb,
            ledger,
            txn_cache,
        }
    }
}

mod subscriptions;
mod transactions_cache;
