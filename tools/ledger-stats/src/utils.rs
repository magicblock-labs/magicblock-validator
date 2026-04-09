use std::path::Path;

use magicblock_ledger::Ledger;

#[allow(dead_code)] // shared source file is compiled by both the lib and bin targets
pub(crate) fn print_two_col_table(
    title: Option<&str>,
    header: [&str; 2],
    rows: &[(String, String)],
) {
    if let Some(title) = title {
        println!("\n++++ {title} ++++\n");
    }

    let left_w = rows
        .iter()
        .map(|(left, _)| left.len())
        .chain(std::iter::once(header[0].len()))
        .max()
        .unwrap_or(header[0].len());
    let right_w = rows
        .iter()
        .map(|(_, right)| right.len())
        .chain(std::iter::once(header[1].len()))
        .max()
        .unwrap_or(header[1].len());

    println!(
        "{:<left_w$}  {:>right_w$}",
        header[0],
        header[1],
        left_w = left_w,
        right_w = right_w
    );
    println!(
        "{:<left_w$}  {:>right_w$}",
        "=".repeat(left_w),
        "=".repeat(right_w),
        left_w = left_w,
        right_w = right_w
    );

    for (left, right) in rows {
        println!(
            "{:<left_w$}  {:>right_w$}",
            left,
            right,
            left_w = left_w,
            right_w = right_w
        );
    }
}

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
