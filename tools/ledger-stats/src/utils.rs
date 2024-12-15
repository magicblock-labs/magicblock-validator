use magicblock_accounts_db::{
    account_storage::AccountStorageEntry, AccountsPersister,
};
use magicblock_ledger::Ledger;
use solana_sdk::clock::Slot;

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

pub(crate) fn accounts_storage_from_ledger(
    ledger: &Ledger,
) -> (AccountStorageEntry, Slot) {
    let accounts_dir = ledger
        .ledger_path()
        .parent()
        .expect("Ledger path has no parent")
        .join("accounts")
        .join("run");
    let persister = AccountsPersister::new_with_paths(vec![accounts_dir]);
    persister.load_most_recent_store().unwrap()
}
