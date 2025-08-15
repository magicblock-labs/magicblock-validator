use std::sync::Arc;

use magicblock_accounts_db::{error::AccountsDbError, StWLock};
use magicblock_bank::{
    bank::Bank, geyser::AccountsUpdateNotifier,
    transaction_logs::TransactionLogCollectorFilter,
    EPHEM_DEFAULT_MILLIS_PER_SLOT,
};
use magicblock_config::AccountsDbConfig;
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
) -> Result<Bank, AccountsDbError> {
    let runtime_config = Arc::new(RuntimeConfig::default());
    let accountsdb_config = AccountsDbConfig::temp_for_tests(500);

    let adb_path = tempfile::tempdir()
        .expect("failed to create temp dir for test bank")
        .keep();
    // for test purposes we don't need to sync with ledger slot, so any slot will do
    let adb_init_slot = u64::MAX;
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
        &adb_path,
        adb_init_slot,
        false,
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
) -> Result<Bank, AccountsDbError> {
    bank_for_tests_with_identity(
        genesis_config,
        accounts_update_notifier,
        slot_status_notifier,
        EPHEM_DEFAULT_MILLIS_PER_SLOT,
        Pubkey::new_unique(),
    )
}
