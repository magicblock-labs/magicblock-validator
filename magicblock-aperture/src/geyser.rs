use std::{fs, path::PathBuf};

use agave_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoV3,
    ReplicaAccountInfoVersions, ReplicaBlockInfoV4, ReplicaBlockInfoVersions,
    ReplicaTransactionInfoV2, ReplicaTransactionInfoVersions, SlotStatus,
};
use json::{JsonValueTrait, Value};
use libloading::{Library, Symbol};
use magicblock_core::link::{
    accounts::AccountWithSlot, blocks::BlockUpdate,
    transactions::TransactionStatus,
};
use solana_account::ReadableAccount;
use solana_transaction_status::RewardsAndNumPartitions;

const ENTRYPOINT_SYMBOL: &[u8] = b"_create_plugin";
#[allow(improper_ctypes_definitions)]
type PluginCreate = unsafe extern "C" fn() -> *mut dyn GeyserPlugin;

pub(crate) struct GeyserPluginManager {
    plugins: Vec<Box<dyn GeyserPlugin>>,
    _libs: Vec<Library>,
}

macro_rules! check_if_enabled {
    ($manager: expr) => {
        if $manager.plugins.is_empty() {
            return Ok(());
        }
    };
}

impl GeyserPluginManager {
    pub(crate) unsafe fn new(
        configs: &[PathBuf],
    ) -> Result<Self, GeyserPluginError> {
        let mut plugins = Vec::with_capacity(configs.len());
        let mut _libs = Vec::with_capacity(configs.len());
        for file_path in configs {
            let config = fs::read_to_string(file_path)?;
            let config: Value = json::from_str(&config).map_err(|e| {
                GeyserPluginError::ConfigFileReadError {
                    msg: format!(
                        "Failed to parse plugin configuration file: {e}"
                    ),
                }
            })?;
            let path = config
                .get("libpath")
                .ok_or(GeyserPluginError::ConfigFileReadError {
                    msg:
                        "Plugin configuration file doesn't contain `path` field"
                            .into(),
                })?
                .as_str()
                .ok_or(GeyserPluginError::ConfigFileReadError {
                    msg:
                        "The `path` field in the configuration must be a string"
                            .into(),
                })?;
            let lib = Library::new(path).map_err(|e| {
                GeyserPluginError::ConfigFileReadError {
                    msg: format!(
                        "Failed to load plugin shared library object file: {e}"
                    ),
                }
            })?;
            let create_plugin: Symbol<PluginCreate> = lib.get(ENTRYPOINT_SYMBOL).map_err(|e| {
                GeyserPluginError::ConfigFileReadError {
                    msg: format!(
                        "Failed to read entry point symbol from plugin object file: {e}"
                    ),
                }
            })?;
            let plugin_raw: *mut dyn GeyserPlugin = create_plugin();
            let mut plugin: Box<dyn GeyserPlugin> = Box::from_raw(plugin_raw);
            plugin.on_load(&file_path.to_string_lossy(), false)?;
            plugin.notify_end_of_startup()?;
            plugins.push(plugin);
            _libs.push(lib);
        }
        Ok(Self { plugins, _libs })
    }

    pub fn notify_account(
        &self,
        data: &AccountWithSlot,
    ) -> Result<(), GeyserPluginError> {
        check_if_enabled!(self);
        let account = &data.account.account;
        let account = ReplicaAccountInfoV3 {
            pubkey: data.account.pubkey.as_array(),
            data: account.data(),
            owner: account.owner().as_array(),
            lamports: account.lamports(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
            // TODO(bmuddha):
            // Syncing account updates with the transactions that mutated them is
            // not trivial and requires significant architectural system changes
            txn: None,
            write_version: 0,
        };
        for plugin in &self.plugins {
            if !plugin.account_data_notifications_enabled() {
                continue;
            }
            let account = ReplicaAccountInfoVersions::V0_0_3(&account);
            plugin.update_account(account, data.slot, false)?;
        }
        Ok(())
    }

    pub fn notify_slot(&self, slot: u64) -> Result<(), GeyserPluginError> {
        check_if_enabled!(self);
        let status = &SlotStatus::Rooted;
        let parent = Some(slot.saturating_sub(1));
        for plugin in &self.plugins {
            plugin.update_slot_status(slot, parent, status)?;
        }
        Ok(())
    }

    pub fn notify_transaction(
        &self,
        txn: &TransactionStatus,
    ) -> Result<(), GeyserPluginError> {
        check_if_enabled!(self);
        let slot = txn.slot;
        let txn = ReplicaTransactionInfoV2 {
            signature: txn.txn.signature(),
            is_vote: false,
            transaction: &txn.txn,
            transaction_status_meta: &txn.meta,
            index: txn.index as usize,
        };
        for plugin in &self.plugins {
            if !plugin.transaction_notifications_enabled() {
                continue;
            }
            let txn = ReplicaTransactionInfoVersions::V0_0_2(&txn);
            plugin.notify_transaction(txn, slot)?;
        }
        Ok(())
    }

    pub fn notify_block(
        &self,
        block: &BlockUpdate,
    ) -> Result<(), GeyserPluginError> {
        check_if_enabled!(self);
        let block = ReplicaBlockInfoV4 {
            slot: block.meta.slot,
            parent_slot: block.meta.slot.strict_sub(1),
            blockhash: &block.hash.to_string(),
            block_height: Some(block.meta.slot),
            rewards: &RewardsAndNumPartitions {
                rewards: Vec::new(),
                num_partitions: None,
            },
            block_time: Some(block.meta.time),
            // TODO(bmuddha): register proper values with the new ledger
            parent_blockhash: "11111111111111111111111111111111",
            executed_transaction_count: 0,
            entry_count: 0,
        };
        for plugin in &self.plugins {
            let block = ReplicaBlockInfoVersions::V0_0_4(&block);
            plugin.notify_block_metadata(block)?;
        }
        Ok(())
    }
}

impl Drop for GeyserPluginManager {
    fn drop(&mut self) {
        for plugin in &mut self.plugins {
            plugin.on_unload();
        }
    }
}
