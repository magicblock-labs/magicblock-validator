use std::path::Path;

use magicblock_accounts_db::AccountsPersister;
use magicblock_ledger::Ledger;
use solana_accounts_db::{
    account_storage::meta::StoredAccountMeta, accounts_db::AccountStorageEntry,
    accounts_file::AccountsFile,
};
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

pub fn all_accounts<R>(
    storage: &AccountStorageEntry,
    cb: impl Fn(StoredAccountMeta) -> R,
) -> Vec<R> {
    let av = match &storage.accounts {
        AccountsFile::AppendVec(av) => av,
        AccountsFile::TieredStorage(_) => {
            unreachable!("we never use tiered accounts storage")
        }
    };
    let mut offset = 0;
    let mut accounts = vec![];
    while let Some((account, next)) =
        av.get_stored_account_meta_callback(offset, |a| {
            let offset = a.offset() + a.stored_size();
            (cb(a), offset)
        })
    {
        accounts.push(account);
        offset = next;
    }
    accounts
}

// TODO use when we need a single account from ledger
pub fn find_account<R>(
    storage: &AccountStorageEntry,
    cb: impl Fn(StoredAccountMeta) -> Option<R>,
) -> Option<R> {
    let av = match &storage.accounts {
        AccountsFile::AppendVec(av) => av,
        AccountsFile::TieredStorage(_) => {
            unreachable!("we never use tiered accounts storage")
        }
    };
    let mut offset = 0;
    let mut account = None;
    while let Some(false) = av.get_stored_account_meta_callback(offset, |a| {
        offset = a.offset() + a.stored_size();
        if let Some(a) = cb(a) {
            account.replace(a);
            true
        } else {
            false
        }
    }) {} // ugly?
    account
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
