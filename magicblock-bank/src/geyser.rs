// TODO(bmuddha): get rid of geyser plugins in validator
// copied from agave-geyser-plugin-manager src/transaction_notifier.rs

use solana_sdk::pubkey::Pubkey;
/// Module responsible for notifying plugins of transactions
use {
    solana_accounts_db::{
        account_storage::meta::StoredAccountMeta,
        accounts_update_notifier_interface::AccountsUpdateNotifierInterface,
    },
    solana_geyser_plugin_interface::geyser_plugin_interface::{
        ReplicaAccountInfoV3, ReplicaAccountInfoVersions,
        ReplicaTransactionInfoV2, ReplicaTransactionInfoVersions,
    },
    solana_geyser_plugin_manager::geyser_plugin_manager::GeyserPluginManager,
    solana_rpc::transaction_notifier_interface::TransactionNotifier as TransactionNotifierInterface,
    solana_sdk::{
        account::{AccountSharedData, ReadableAccount},
        clock::Slot,
        signature::Signature,
        transaction::SanitizedTransaction,
    },
    solana_transaction_status::TransactionStatusMeta,
    std::sync::{Arc, RwLock},
};

/// This implementation of TransactionNotifier is passed to the rpc's TransactionStatusService
/// at the validator startup. TransactionStatusService invokes the notify_transaction method
/// for new transactions. The implementation in turn invokes the notify_transaction of each
/// plugin enabled with transaction notification managed by the GeyserPluginManager.
pub struct TransactionNotifier {
    plugin_manager: Arc<RwLock<GeyserPluginManager>>,
}

impl TransactionNotifierInterface for TransactionNotifier {
    fn notify_transaction(
        &self,
        slot: Slot,
        index: usize,
        signature: &Signature,
        transaction_status_meta: &TransactionStatusMeta,
        transaction: &SanitizedTransaction,
    ) {
        let transaction_log_info = Self::build_replica_transaction_info(
            index,
            signature,
            transaction_status_meta,
            transaction,
        );

        let plugin_manager = self.plugin_manager.read().unwrap();

        if plugin_manager.plugins.is_empty() {
            return;
        }

        for plugin in plugin_manager.plugins.iter() {
            if !plugin.transaction_notifications_enabled() {
                continue;
            }
            let _ = plugin.notify_transaction(
                ReplicaTransactionInfoVersions::V0_0_2(&transaction_log_info),
                slot,
            );
        }
    }
}

impl TransactionNotifier {
    pub fn new(plugin_manager: Arc<RwLock<GeyserPluginManager>>) -> Self {
        Self { plugin_manager }
    }

    fn build_replica_transaction_info<'a>(
        index: usize,
        signature: &'a Signature,
        transaction_status_meta: &'a TransactionStatusMeta,
        transaction: &'a SanitizedTransaction,
    ) -> ReplicaTransactionInfoV2<'a> {
        ReplicaTransactionInfoV2 {
            index,
            signature,
            is_vote: transaction.is_simple_vote_transaction(),
            transaction,
            transaction_status_meta,
        }
    }
}

#[derive(Debug)]
pub struct AccountsUpdateNotifier {
    plugin_manager: Arc<RwLock<GeyserPluginManager>>,
}

impl AccountsUpdateNotifierInterface for AccountsUpdateNotifier {
    fn snapshot_notifications_enabled(&self) -> bool {
        false
    }

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
            let _ = plugin
                .update_account(
                    ReplicaAccountInfoVersions::V0_0_3(&account),
                    slot,
                    is_startup,
                )
                .inspect_err(|err| {
                    log::error!(
                        "failed to notify plugin of account update: {err}"
                    )
                });
        }
    }
}
