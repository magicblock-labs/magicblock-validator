use log::*;
use solana_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, ReplicaAccountInfoVersions, ReplicaBlockInfoVersions,
    ReplicaEntryInfoVersions, ReplicaTransactionInfoVersions, Result,
    SlotStatus,
};
use solana_sdk::{clock::Slot, pubkey::Pubkey};

#[derive(Debug)]
pub struct GrpcGeyserPlugin;

#[allow(unused)]
impl GeyserPlugin for GrpcGeyserPlugin {
    fn name(&self) -> &'static str {
        concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"))
    }

    fn on_load(&mut self, _config_file: &str, _is_reload: bool) -> Result<()> {
        debug!("Loading plugin");
        Ok(())
    }

    fn on_unload(&mut self) {
        debug!("Unloading plugin");
    }

    fn update_account(
        &self,
        account: ReplicaAccountInfoVersions,
        slot: Slot,
        is_startup: bool,
    ) -> Result<()> {
        let account = match account {
            ReplicaAccountInfoVersions::V0_0_1(_info) => {
                unreachable!(
                    "ReplicaAccountInfoVersions::V0_0_1 is not supported"
                )
            }
            ReplicaAccountInfoVersions::V0_0_2(_info) => {
                unreachable!(
                    "ReplicaAccountInfoVersions::V0_0_2 is not supported"
                )
            }
            ReplicaAccountInfoVersions::V0_0_3(info) => info,
        };

        if account.txn.is_some() {
            debug!(
                "update_account '{}': {:?}",
                Pubkey::try_from(account.pubkey).unwrap(),
                account
            );
        }
        Ok(())
    }

    fn notify_end_of_startup(&self) -> Result<()> {
        debug!("End of startup");
        Ok(())
    }

    fn update_slot_status(
        &self,
        slot: Slot,
        parent: Option<u64>,
        status: SlotStatus,
    ) -> Result<()> {
        Ok(())
    }

    fn notify_transaction(
        &self,
        transaction: ReplicaTransactionInfoVersions,
        slot: Slot,
    ) -> Result<()> {
        let transaction = match transaction {
            ReplicaTransactionInfoVersions::V0_0_1(_info) => {
                unreachable!(
                    "ReplicaAccountInfoVersions::V0_0_1 is not supported"
                )
            }
            ReplicaTransactionInfoVersions::V0_0_2(info) => info,
        };

        debug!("notify_transaction: {:?}", transaction);
        Ok(())
    }

    fn notify_entry(&self, entry: ReplicaEntryInfoVersions) -> Result<()> {
        Ok(())
    }

    fn notify_block_metadata(
        &self,
        blockinfo: ReplicaBlockInfoVersions,
    ) -> Result<()> {
        Ok(())
    }

    fn account_data_notifications_enabled(&self) -> bool {
        true
    }

    fn transaction_notifications_enabled(&self) -> bool {
        true
    }

    fn entry_notifications_enabled(&self) -> bool {
        false
    }
}
