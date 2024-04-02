use std::path::PathBuf;
use std::sync::Arc;

use sleipnir_bank::{
    bank::Bank, transaction_logs::TransactionLogCollectorFilter,
};
use solana_accounts_db::{
    accounts_db::{AccountShrinkThreshold, ACCOUNTS_DB_CONFIG_FOR_TESTING},
    accounts_index::AccountSecondaryIndexes,
    accounts_update_notifier_interface::AccountsUpdateNotifier,
};
use solana_sdk::{genesis_config::GenesisConfig, pubkey::Pubkey};
use solana_svm::runtime_config::RuntimeConfig;

// Lots is almost duplicate of /Volumes/d/dev/mb/validator/x-validator/sleipnir-bank/src/bank_dev_utils/bank.rs
// in order to make it accessible without needing the feature flag

// Special case allowing to pass in paths which are needed for accounts db
// updating geyser accounts updates notifier
pub fn bank_for_tests_with_paths(
    genesis_config: &GenesisConfig,
    accounts_update_notifier: Option<AccountsUpdateNotifier>,
    paths: Vec<&str>,
) -> Bank {
    let shrink_ratio = AccountShrinkThreshold::default();

    let runtime_config = Arc::new(RuntimeConfig::default());
    let account_indexes = AccountSecondaryIndexes::default();

    let paths = paths.into_iter().map(PathBuf::from).collect();
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
        accounts_update_notifier,
        Some(Pubkey::new_unique()),
        Arc::default(),
    );
    bank.transaction_log_collector_config
        .write()
        .unwrap()
        .filter = TransactionLogCollectorFilter::All;
    bank
}

pub fn bank_for_tests(
    genesis_config: &GenesisConfig,
    accounts_update_notifier: Option<AccountsUpdateNotifier>,
) -> Bank {
    bank_for_tests_with_paths(genesis_config, accounts_update_notifier, vec![])
}
