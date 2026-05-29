use std::{sync::Arc, time::Duration};

use magicblock_accounts_db::AccountsDb;
use magicblock_ledger::Ledger;
use magicblock_metrics::metrics;
use tokio_util::sync::CancellationToken;
use tracing::*;

#[allow(unused_variables)]
pub fn init_system_metrics_ticker(
    tick_duration: Duration,
    ledger: &Arc<Ledger>,
    accountsdb: &Arc<AccountsDb>,
    token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    fn try_set_ledger_storage_size(ledger: &Ledger) {
        match ledger.storage_size() {
            Ok(byte_size) => metrics::set_ledger_size(byte_size),
            Err(err) => {
                warn!(error = ?err, "Failed to get ledger storage size")
            }
        }
    }
    fn set_accounts_storage_size(accounts_db: &AccountsDb) {
        let byte_size = accounts_db.storage_size();
        metrics::set_accounts_size(byte_size.try_into().unwrap_or(i64::MAX));
    }
    fn set_accounts_count(accounts_db: &AccountsDb) {
        metrics::set_accounts_count(
            accounts_db.account_count().try_into().unwrap_or(i64::MAX),
        );
    }

    let ledger = ledger.clone();
    let bank = accountsdb.clone();
    tokio::task::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick_duration) => {
                    try_set_ledger_storage_size(&ledger);
                    set_accounts_storage_size(&bank);
                    set_accounts_count(&bank);
                },
                _ = token.cancelled() => {
                    break;
                }
            }
        }

        info!("System metrics ticker shutdown");
    })
}
