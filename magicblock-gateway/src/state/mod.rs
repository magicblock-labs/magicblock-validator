use std::sync::Arc;

use magicblock_accounts_db::AccountsDb;
use magicblock_ledger::Ledger;
use subscriptions::SubscriptionsDb;
use transactions::TransactionsCache;

#[derive(Clone)]
pub struct SharedState {
    pub(crate) accountsdb: Arc<AccountsDb>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) transactions: Arc<TransactionsCache>,
    pub(crate) subscriptions: SubscriptionsDb,
}

impl SharedState {
    fn new(accountsdb: Arc<AccountsDb>, ledger: Arc<Ledger>) -> Self {
        let transactions = TransactionsCache::init();
        let subscriptions = SubscriptionsDb::default();
        Self {
            accountsdb,
            ledger,
            transactions,
            subscriptions,
        }
    }
}

pub(crate) mod subscriptions;
pub(crate) mod transactions;
