use std::sync::Arc;

use magicblock_accounts_db::{
    config::AccountsDbConfig, error::AccountsDbError, StWLock,
};
use magicblock_bank::{
    bank::Bank, geyser::AccountsUpdateNotifier,
    transaction_logs::TransactionLogCollectorFilter,
    EPHEM_DEFAULT_MILLIS_PER_SLOT,
};
use solana_geyser_plugin_manager::slot_status_notifier::SlotStatusNotifierImpl;
use solana_sdk::{genesis_config::GenesisConfig, pubkey::Pubkey};
use solana_svm::runtime_config::RuntimeConfig;

// Lots is almost duplicate of bank/src/bank_dev_utils/bank.rs
// in order to make it accessible without needing the feature flag

// Special case for test allowing to pass validator identity
pub fn bank_for_tests_with_identity(
    genesis_config: &GenesisConfig,
    accounts_update_notifier: Option<AccountsUpdateNotifier>,
    slot_status_notifier: Option<SlotStatusNotifierImpl>,
    millis_per_slot: u64,
    identity_id: Pubkey,
) -> std::result::Result<Bank, AccountsDbError> {
    let runtime_config = Arc::new(RuntimeConfig::default());
    let accountsdb_config = AccountsDbConfig::temp_for_tests(500);
    let bank = Bank::new(
        genesis_config,
        runtime_config,
        &accountsdb_config,
        None,
        None,
        false,
        accounts_update_notifier,
        slot_status_notifier,
        millis_per_slot,
        identity_id,
        // TODO(bmuddha): when we switch to multithreaded mode,
        // switch to actual lock held by scheduler
        StWLock::default(),
    )?;
    bank.transaction_log_collector_config
        .write()
        .unwrap()
        .filter = TransactionLogCollectorFilter::All;
    Ok(bank)
}

pub fn bank_for_tests(
    genesis_config: &GenesisConfig,
    accounts_update_notifier: Option<AccountsUpdateNotifier>,
    slot_status_notifier: Option<SlotStatusNotifierImpl>,
) -> std::result::Result<Bank, AccountsDbError> {
    bank_for_tests_with_identity(
        genesis_config,
        accounts_update_notifier,
        slot_status_notifier,
        EPHEM_DEFAULT_MILLIS_PER_SLOT,
        Pubkey::new_unique(),
    )
}
