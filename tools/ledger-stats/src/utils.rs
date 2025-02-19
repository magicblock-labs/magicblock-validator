use std::path::Path;

use magicblock_ledger::Ledger;
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

pub fn open_ledger(ledger_path: &Path) -> Ledger {
    Ledger::open(ledger_path).expect("Failed to open ledger")
}
