use std::sync::Arc;

use log::*;
use magicblock_config::GeyserGrpcConfig;
use magicblock_geyser_plugin::{
    config::{
        Config as GeyserPluginConfig, ConfigGrpc as GeyserPluginConfigGrpc,
    },
    plugin::GrpcGeyserPlugin,
    rpc::GeyserRpcService,
};
use solana_geyser_plugin_interface::geyser_plugin_interface::GeyserPlugin;
use solana_geyser_plugin_manager::{
    geyser_plugin_manager::LoadedGeyserPlugin,
    geyser_plugin_service::{GeyserPluginService, GeyserPluginServiceError},
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
    pub geyser_plugins: Option<Vec<LoadedGeyserPlugin>>,
    pub geyser_grpc: GeyserGrpcConfig,
}

impl Default for InitGeyserServiceConfig {
    fn default() -> Self {
        Self {
            cache_accounts: true,
            cache_transactions: true,
            enable_account_notifications: true,
            enable_transaction_notifications: true,
            geyser_plugins: None,
            geyser_grpc: Default::default(),
        }
    }
}

impl InitGeyserServiceConfig {
    pub fn add_plugin(&mut self, name: String, plugin: Box<dyn GeyserPlugin>) {
        self.add_loaded_plugin(LoadedGeyserPlugin::new(plugin, Some(name)));
    }

    pub fn add_loaded_plugin(&mut self, plugin: LoadedGeyserPlugin) {
        self.geyser_plugins
            .get_or_insert_with(Vec::new)
            .push(plugin);
    }
}

// -----------------
// init_geyser_service
// -----------------
pub fn init_geyser_service(
    config: InitGeyserServiceConfig,
) -> Result<
    (GeyserPluginService, Arc<GeyserRpcService>),
    GeyserPluginServiceError,
> {
    let InitGeyserServiceConfig {
        cache_accounts,
        cache_transactions,
        enable_account_notifications,
        enable_transaction_notifications,
        geyser_plugins,
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
    let (grpc_plugin, rpc_service) = {
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
        (LoadedGeyserPlugin::new(Box::new(plugin), None), rpc_service)
    };

    // vec combined with grpc_plubin
    let plugins = match geyser_plugins {
        Some(mut plugins) => {
            plugins.push(grpc_plugin);
            plugins
        }
        None => vec![grpc_plugin],
    };
    let geyser_service = GeyserPluginService::new(&[], plugins)?;
    Ok((geyser_service, rpc_service))
}
