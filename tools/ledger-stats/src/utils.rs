use std::path::Path;

use magicblock_accounts_db::AccountsPersister;
use magicblock_ledger::Ledger;
use solana_accounts_db::accounts_db::AccountStorageEntry;
use solana_sdk::clock::Slot;

#[allow(dead_code)]
pub fn render_logs(logs: &[String], indent: &str) -> String {
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
