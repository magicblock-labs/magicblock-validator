// TODO(bmuddha): purge geyser plugin subsystem from validator completely!
// copied from agave-geyser-plugin-manager src/accounts_update_notifier.rs

use std::sync::{Arc, RwLock};

use solana_accounts_db::{
    account_storage::meta::StoredAccountMeta,
    accounts_update_notifier_interface::AccountsUpdateNotifierInterface,
};
use solana_geyser_plugin_interface::geyser_plugin_interface::{
    ReplicaAccountInfoV3, ReplicaAccountInfoVersions,
};
use solana_geyser_plugin_manager::geyser_plugin_manager::GeyserPluginManager;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    clock::Slot,
    pubkey::Pubkey,
    transaction::SanitizedTransaction,
};

#[derive(Debug)]
pub struct AccountsUpdateNotifier {
    plugin_manager: Arc<RwLock<GeyserPluginManager>>,
}

impl AccountsUpdateNotifierInterface for AccountsUpdateNotifier {
    fn notify_account_update(
        &self,
        slot: Slot,
        account: &AccountSharedData,
        txn: &Option<&SanitizedTransaction>,
        pubkey: &Pubkey,
        write_version: u64,
    ) {
        let account_info = self.accountinfo_from_shared_account_data(
            account,
            txn,
            pubkey,
            write_version,
        );
        self.notify_plugins_of_account_update(account_info, slot, false);
    }

    fn notify_account_restore_from_snapshot(
        &self,
        slot: Slot,
        account: &StoredAccountMeta,
    ) {
        let account = self.accountinfo_from_stored_account_meta(account);
        self.notify_plugins_of_account_update(account, slot, true);
    }

    fn notify_end_of_restore_from_snapshot(&self) {
        let plugin_manager = self.plugin_manager.read().unwrap();
        if plugin_manager.plugins.is_empty() {
            return;
        }

        for plugin in plugin_manager.plugins.iter() {
            let _ = plugin.notify_end_of_startup();
        }
    }
}

impl AccountsUpdateNotifier {
    pub fn new(plugin_manager: Arc<RwLock<GeyserPluginManager>>) -> Self {
        Self { plugin_manager }
    }

    fn accountinfo_from_shared_account_data<'a>(
        &self,
        account: &'a AccountSharedData,
        txn: &'a Option<&'a SanitizedTransaction>,
        pubkey: &'a Pubkey,
        write_version: u64,
    ) -> ReplicaAccountInfoV3<'a> {
        ReplicaAccountInfoV3 {
            pubkey: pubkey.as_ref(),
            lamports: account.lamports(),
            owner: account.owner().as_ref(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
            data: account.data(),
            write_version,
            txn: *txn,
        }
    }

    fn accountinfo_from_stored_account_meta<'a>(
        &self,
        stored_account_meta: &'a StoredAccountMeta,
    ) -> ReplicaAccountInfoV3<'a> {
        // We do not need to rely on the specific write_version read from the append vec.
        // So, overwrite the write_version with something that works.
        // There is already only entry per pubkey.
        // write_version is only used to order multiple entries with the same pubkey,
        // so it doesn't matter what value it gets here.
        // Passing 0 for everyone's write_version is sufficiently correct.
        let write_version = 0;
        ReplicaAccountInfoV3 {
            pubkey: stored_account_meta.pubkey().as_ref(),
            lamports: stored_account_meta.lamports(),
            owner: stored_account_meta.owner().as_ref(),
            executable: stored_account_meta.executable(),
            rent_epoch: stored_account_meta.rent_epoch(),
            data: stored_account_meta.data(),
            write_version,
            txn: None,
        }
    }

    fn notify_plugins_of_account_update(
        &self,
        account: ReplicaAccountInfoV3,
        slot: Slot,
        is_startup: bool,
    ) {
        let plugin_manager = self.plugin_manager.read().unwrap();

        if plugin_manager.plugins.is_empty() {
            return;
        }
        for plugin in plugin_manager.plugins.iter() {
            let _ = plugin.update_account(
                ReplicaAccountInfoVersions::V0_0_3(&account),
                slot,
                is_startup,
            );
        }
    }
}
