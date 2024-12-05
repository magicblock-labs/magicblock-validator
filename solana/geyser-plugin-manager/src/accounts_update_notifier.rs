/// Module responsible for notifying plugins of account updates
use {
    crate::geyser_plugin_manager::GeyserPluginManager,
    log::*,
    magicblock_accounts_db::accounts_update_notifier_interface::AccountsUpdateNotifierInterface,
    solana_geyser_plugin_interface::geyser_plugin_interface::{
        ReplicaAccountInfoV3, ReplicaAccountInfoVersions,
    },
    solana_measure::measure::Measure,
    solana_metrics::*,
    solana_sdk::{
        account::{AccountSharedData, ReadableAccount},
        clock::Slot,
        pubkey::Pubkey,
        transaction::SanitizedTransaction,
    },
    std::sync::{Arc, RwLock},
};
#[derive(Debug)]
pub(crate) struct AccountsUpdateNotifierImpl {
    plugin_manager: Arc<RwLock<GeyserPluginManager>>,
}

impl AccountsUpdateNotifierInterface for AccountsUpdateNotifierImpl {
    fn notify_account_update(
        &self,
        slot: Slot,
        account: &AccountSharedData,
        txn: &Option<&SanitizedTransaction>,
        pubkey: &Pubkey,
        write_version: u64,
    ) {
        if let Some(account_info) = self.accountinfo_from_shared_account_data(
            account,
            txn,
            pubkey,
            write_version,
        ) {
            self.notify_plugins_of_account_update(account_info, slot, false);
        }
    }
}

impl AccountsUpdateNotifierImpl {
    pub fn new(plugin_manager: Arc<RwLock<GeyserPluginManager>>) -> Self {
        AccountsUpdateNotifierImpl { plugin_manager }
    }

    fn accountinfo_from_shared_account_data<'a>(
        &self,
        account: &'a AccountSharedData,
        txn: &'a Option<&'a SanitizedTransaction>,
        pubkey: &'a Pubkey,
        write_version: u64,
    ) -> Option<ReplicaAccountInfoV3<'a>> {
        Some(ReplicaAccountInfoV3 {
            pubkey: pubkey.as_ref(),
            lamports: account.lamports(),
            owner: account.owner().as_ref(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
            data: account.data(),
            write_version,
            txn: *txn,
        })
    }

    fn notify_plugins_of_account_update(
        &self,
        account: ReplicaAccountInfoV3,
        slot: Slot,
        is_startup: bool,
    ) {
        let mut measure2 =
            Measure::start("geyser-plugin-notify_plugins_of_account_update");
        let plugin_manager = self.plugin_manager.read().unwrap();

        if plugin_manager.plugins.is_empty() {
            return;
        }
        for plugin in plugin_manager.plugins.iter() {
            let mut measure = Measure::start("geyser-plugin-update-account");
            match plugin.update_account(
                ReplicaAccountInfoVersions::V0_0_3(&account),
                slot,
                is_startup,
            ) {
                Err(err) => {
                    error!(
                        "Failed to update account {} at slot {}, error: {} to plugin {}",
                        bs58::encode(account.pubkey).into_string(),
                        slot,
                        err,
                        plugin.name()
                    )
                }
                Ok(_) => {
                    trace!(
                        "Successfully updated account {} at slot {} to plugin {}",
                        bs58::encode(account.pubkey).into_string(),
                        slot,
                        plugin.name()
                    );
                }
            }
            measure.stop();
            inc_new_counter_debug!(
                "geyser-plugin-update-account-us",
                measure.as_us() as usize,
                100000,
                100000
            );
        }
        measure2.stop();
        inc_new_counter_debug!(
            "geyser-plugin-notify_plugins_of_account_update-us",
            measure2.as_us() as usize,
            100000,
            100000
        );
    }
}
