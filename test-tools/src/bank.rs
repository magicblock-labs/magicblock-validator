use std::sync::Arc;

use sleipnir_accounts_db::accounts_update_notifier_interface::AccountsUpdateNotifier;
use sleipnir_bank::{
    bank::Bank, slot_status_notifier_interface::SlotStatusNotifierArc,
    transaction_logs::TransactionLogCollectorFilter,
    EPHEM_DEFAULT_MILLIS_PER_SLOT,
};
use solana_sdk::{genesis_config::GenesisConfig, pubkey::Pubkey};
use solana_svm::runtime_config::RuntimeConfig;

// Lots is almost duplicate of /Volumes/d/dev/mb/validator/x-validator/sleipnir-bank/src/bank_dev_utils/bank.rs
// in order to make it accessible without needing the feature flag

// Special case for test allowing to pass validator identity
pub fn bank_for_tests_with_identity(
    genesis_config: &GenesisConfig,
    accounts_update_notifier: Option<AccountsUpdateNotifier>,
    slot_status_notifier: Option<SlotStatusNotifierArc>,
    millis_per_slot: u64,
    identity_id: Pubkey,
) -> Bank {
    let runtime_config = Arc::new(RuntimeConfig::default());
    let bank = Bank::new(
        genesis_config,
        runtime_config,
        None,
        None,
        false,
        accounts_update_notifier,
        slot_status_notifier,
        millis_per_slot,
        identity_id,
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
    slot_status_notifier: Option<SlotStatusNotifierArc>,
) -> Bank {
    bank_for_tests_with_identity(
        genesis_config,
        accounts_update_notifier,
        slot_status_notifier,
        EPHEM_DEFAULT_MILLIS_PER_SLOT,
        Pubkey::new_unique(),
    )
}
