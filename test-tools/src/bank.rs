use std::sync::Arc;

use sleipnir_bank::{
    bank::Bank, transaction_logs::TransactionLogCollectorFilter,
};
use solana_accounts_db::{
    accounts_db::{AccountShrinkThreshold, ACCOUNTS_DB_CONFIG_FOR_TESTING},
    accounts_index::AccountSecondaryIndexes,
};
use solana_sdk::{genesis_config::GenesisConfig, pubkey::Pubkey};
use solana_svm::runtime_config::RuntimeConfig;

// Lots is almost duplicate of /Volumes/d/dev/mb/validator/x-validator/sleipnir-bank/src/bank_dev_utils/bank.rs
// in order to make it accessible without needing the feature flag
pub fn bank_for_tests(genesis_config: &GenesisConfig) -> Bank {
    let shrink_ratio = AccountShrinkThreshold::default();

    let runtime_config = Arc::new(RuntimeConfig::default());
    let paths = Vec::new();
    let account_indexes = AccountSecondaryIndexes::default();

    let bank = Bank::new_with_paths(
        genesis_config,
        runtime_config,
        paths,
        None,
        None,
        account_indexes,
        shrink_ratio,
        false,
        Some(ACCOUNTS_DB_CONFIG_FOR_TESTING),
        None,
        Some(Pubkey::new_unique()),
        Arc::default(),
    );
    bank.transaction_log_collector_config
        .write()
        .unwrap()
        .filter = TransactionLogCollectorFilter::All;
    bank
}
