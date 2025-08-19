use std::sync::Arc;

use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::transactions::TransactionSchedulerHandle;

struct ExecutionTestEnv {
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    transaction_scheduler: TransactionSchedulerHandle,
}
