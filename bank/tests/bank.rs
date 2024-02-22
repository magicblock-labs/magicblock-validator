mod utils;

use std::collections::HashSet;

use log::{debug, info, trace};
use sleipnir_bank::bank::{Bank, TransactionExecutionRecordingOpts};
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    clock::MAX_PROCESSING_AGE, genesis_config::create_genesis_config, system_program,
};

use crate::utils::init_logger;

#[test]
fn test_bank_one_system_instruction() {
    init_logger();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);

    let txs = utils::create_transactions(&bank, 1);
    let batch = bank.prepare_sanitized_batch(&txs);

    let mut timings = ExecuteTimings::default();
    let (transaction_results, transaction_balances) = bank.load_execute_and_commit_transactions(
        &batch,
        MAX_PROCESSING_AGE,
        true,
        TransactionExecutionRecordingOpts::recording_logs(),
        &mut timings,
        None,
    );

    trace!("{:#?}", txs);
    trace!("{:#?}", transaction_results.execution_results);
    debug!("{:#?}", transaction_balances);

    for key in txs
        .iter()
        .flat_map(|tx| tx.message().account_keys().iter())
        .collect::<HashSet<_>>()
    {
        if key.eq(&system_program::id()) {
            continue;
        }

        let account = bank.get_account(key).unwrap();

        debug!("{:?}: {:#?}", key, account);
    }

    info!("=============== Logs ===============");
    for res in transaction_results.execution_results.iter() {
        if let Some(logs) = res.details().as_ref().and_then(|x| x.log_messages.as_ref()) {
            for log in logs {
                info!("> {log}");
            }
        }
    }
}
