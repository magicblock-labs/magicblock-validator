use std::{fs, path::PathBuf};

use agave_geyser_plugin_interface::geyser_plugin_interface::{
    GeyserPlugin, GeyserPluginError, ReplicaAccountInfoV3,
    ReplicaAccountInfoVersions, SlotStatus,
};
use json::{JsonValueTrait, Value};
use libloading::{Library, Symbol};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;

const ENTRYPOINT_SYMBOL: &[u8] = b"_create_plugin";
#[allow(improper_ctypes_definitions)]
type PluginCreate = unsafe extern "C" fn() -> *mut dyn GeyserPlugin;

pub(crate) struct GeyserPluginManager {
    plugins: Vec<Box<dyn GeyserPlugin>>,
    _libs: Vec<Library>,
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
                .get("path")
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
        pubkey: &Pubkey,
        account: &AccountSharedData,
        slot: u64,
    ) -> Result<(), GeyserPluginError> {
        let account = ReplicaAccountInfoV3 {
            pubkey: pubkey.as_array(),
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
            plugin.update_account(account, slot, false)?;
        }
        Ok(())
    }

    pub fn notify_slot(&self, slot: u64) -> Result<(), GeyserPluginError> {
        let status = &SlotStatus::Rooted;
        let parent = Some(slot.saturating_sub(1));
        for plugin in &self.plugins {
            plugin.update_slot_status(slot, parent, status)?;
        }
        Ok(())
    }
}
