use log::info;
use solana_accounts_db::transaction_results::TransactionExecutionDetails;

pub fn init_logger() {
    let _ = env_logger::builder()
        .format_timestamp_micros()
        .is_test(true)
        .try_init();
}

pub fn log_exec_details(transaction_results: &TransactionExecutionDetails) {
    info!("");
    info!("=============== Logs ===============");
    if let Some(logs) = transaction_results.log_messages.as_ref() {
        for log in logs {
            info!("> {log}");
        }
    }
    info!("");
}
