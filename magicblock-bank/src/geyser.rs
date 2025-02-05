// TODO(bmuddha): get rid of geyser plugins in validator
// copied from agave-geyser-plugin-manager src/transaction_notifier.rs

/// Module responsible for notifying plugins of transactions
use {
    solana_geyser_plugin_interface::geyser_plugin_interface::{
        ReplicaTransactionInfoV2, ReplicaTransactionInfoVersions,
    },
    solana_geyser_plugin_manager::geyser_plugin_manager::GeyserPluginManager,
    solana_rpc::transaction_notifier_interface::TransactionNotifier,
    solana_sdk::{
        clock::Slot, signature::Signature, transaction::SanitizedTransaction,
    },
    solana_transaction_status::TransactionStatusMeta,
    std::sync::{Arc, RwLock},
};

/// This implementation of TransactionNotifier is passed to the rpc's TransactionStatusService
/// at the validator startup. TransactionStatusService invokes the notify_transaction method
/// for new transactions. The implementation in turn invokes the notify_transaction of each
/// plugin enabled with transaction notification managed by the GeyserPluginManager.
pub struct TransactionNotifierImpl {
    plugin_manager: Arc<RwLock<GeyserPluginManager>>,
}

impl TransactionNotifier for TransactionNotifierImpl {
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

impl TransactionNotifierImpl {
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
