mod utils;

use sleipnir_bank::bank::Bank;
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{clock::MAX_PROCESSING_AGE, genesis_config::create_genesis_config};

use crate::utils::init_logger;

#[test]
fn test_bank_one_system_instruction() {
    init_logger();

    let (genesis_config, _) = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config);

    let txs = utils::create_transactions(&bank, 1);
    let batch = bank.prepare_sanitized_batch(&txs);

    let mut timings = ExecuteTimings::default();
    let res = bank.load_and_execute_transactions(
        &batch,
        MAX_PROCESSING_AGE,
        Default::default(),
        &mut timings,
        None,
        None,
    );
    eprintln!("{:#?}", res.loaded_transactions);
}
