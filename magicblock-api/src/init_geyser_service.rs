use std::sync::Arc;

use libloading::Library;
use log::*;
use magicblock_config::GeyserGrpcConfig;
use magicblock_geyser_plugin::{
    config::{
        Config as GeyserPluginConfig, ConfigGrpc as GeyserPluginConfigGrpc,
    },
    plugin::GrpcGeyserPlugin,
    rpc::GeyserRpcService,
};
use solana_geyser_plugin_manager::{
    geyser_plugin_manager::{GeyserPluginManager, LoadedGeyserPlugin},
    geyser_plugin_service::GeyserPluginServiceError,
};

// -----------------
// InitGeyserServiceConfig
// -----------------
#[derive(Debug)]
pub struct InitGeyserServiceConfig {
    pub cache_accounts: bool,
    pub cache_transactions: bool,
    pub enable_account_notifications: bool,
    pub enable_transaction_notifications: bool,
    pub geyser_grpc: GeyserGrpcConfig,
}

impl Default for InitGeyserServiceConfig {
    fn default() -> Self {
        Self {
            cache_accounts: true,
            cache_transactions: true,
            enable_account_notifications: true,
            enable_transaction_notifications: true,
            geyser_grpc: Default::default(),
        }
    }
}

// -----------------
// init_geyser_service
// -----------------
pub fn init_geyser_service(
    config: InitGeyserServiceConfig,
) -> Result<
    (GeyserPluginManager, Arc<GeyserRpcService>),
    GeyserPluginServiceError,
> {
    let InitGeyserServiceConfig {
        cache_accounts,
        cache_transactions,
        enable_account_notifications,
        enable_transaction_notifications,
        geyser_grpc,
    } = config;

    let config = GeyserPluginConfig {
        cache_accounts,
        cache_transactions,
        enable_account_notifications,
        enable_transaction_notifications,
        grpc: GeyserPluginConfigGrpc::default_with_addr(
            geyser_grpc.socket_addr(),
        ),
        ..Default::default()
    };
    let mut manager = GeyserPluginManager::new();
    let (plugin, rpc_service) = {
        let plugin = GrpcGeyserPlugin::create(config)
            .map_err(|err| {
                error!("Failed to load geyser plugin: {:?}", err);
                err
            })
            .unwrap_or_else(|_| {
                panic!(
                    "Failed to launch GRPC Geyser service on '{}'",
                    geyser_grpc.socket_addr()
                )
            });
        info!(
            "Launched GRPC Geyser service on '{}'",
            geyser_grpc.socket_addr()
        );
        let rpc_service = plugin.rpc();
        // hack: we don't load the geyser plugin from .so file, as such we don't own a handle to
        // Library, to bypass this, we just make up one from a pointer to a leaked 8 byte memory,
        // and forget about it, this should work as long as geyser plugin manager doesn't try to do
        // anything fancy with that handle, and when drop method of the Library is called, nothing
        // bad happens if the address is the garbage, as long as it's not null
        // (admittedly ugly solution)
        let dummy = Box::leak(Box::new(0usize)) as *const usize;
        let lib =
            unsafe { std::mem::transmute::<*const usize, Library>(dummy) };
        (
            LoadedGeyserPlugin::new(lib, Box::new(plugin), None),
            rpc_service,
        )
    };
    manager.plugins.push(plugin);

    Ok((manager, rpc_service))
}
