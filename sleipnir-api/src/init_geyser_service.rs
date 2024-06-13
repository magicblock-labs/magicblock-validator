use log::*;
use std::sync::Arc;

use sleipnir_geyser_plugin::{
    config::Config as GeyserPluginConfig, plugin::GrpcGeyserPlugin,
    rpc::GeyserRpcService,
};
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
}

impl Default for InitGeyserServiceConfig {
    fn default() -> Self {
        Self {
            cache_accounts: true,
            cache_transactions: true,
            enable_account_notifications: true,
            enable_transaction_notifications: true,
            geyser_plugins: None,
        }
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
    } = config;

    let config = GeyserPluginConfig {
        cache_accounts,
        cache_transactions,
        enable_account_notifications,
        enable_transaction_notifications,
        ..Default::default()
    };
    let (grpc_plugin, rpc_service) = {
        let plugin = GrpcGeyserPlugin::create(config)
            .map_err(|err| {
                error!("Failed to load geyser plugin: {:?}", err);
                err
            })
            .expect("Failed to load grpc geyser plugin");
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
