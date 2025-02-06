use std::path::Path;

use magicblock_accounts_db::AccountsPersister;
use magicblock_ledger::Ledger;
use solana_accounts_db::{
    account_storage::meta::StoredAccountMeta, accounts_db::AccountStorageEntry,
};
use solana_sdk::{account::AccountSharedData, clock::Slot};

#[allow(dead_code)] // this is actually used from `print_transaction_logs` ./transaction_logs.rs
pub(crate) fn render_logs(logs: &[String], indent: &str) -> String {
    logs.iter()
        .map(|line| {
            let prefix =
                if line.contains("Program") && line.contains("invoke [") {
                    format!("\n{indent}")
                } else {
                    format!("{indent}{indent}• ")
                };
            format!("{prefix}{line}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn accounts_storage_from_ledger(
    ledger: &Ledger,
) -> (AccountStorageEntry, Slot) {
    let accounts_dir = ledger
        .ledger_path()
        .parent()
        .expect("Ledger path has no parent")
        .join("accounts")
        .join("run");
    let persister = AccountsPersister::new_with_paths(vec![accounts_dir]);
    persister
        .load_most_recent_store(u64::MAX)
        .unwrap()
        .expect("No recent store found")
}

pub fn open_ledger(ledger_path: &Path) -> Ledger {
    Ledger::open(ledger_path).expect("Failed to open ledger")
}

// TODO just a placeholder function to make the project compile
pub fn all_accounts<R>(
    storage: &AccountStorageEntry,
    cb: impl Fn(StoredAccountMeta) -> R,
) -> Vec<R> {
    unimplemented!()
}
// TODO just a placeholder function to make the project compile
pub fn find_account<R>(
    storage: &AccountStorageEntry,
    cb: impl Fn(StoredAccountMeta) -> R,
) -> R {
    unimplemented!()
}
