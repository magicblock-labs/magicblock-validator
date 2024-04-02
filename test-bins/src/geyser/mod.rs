use sleipnir_geyser_plugin::plugin::GrpcGeyserPlugin;
use solana_geyser_plugin_manager::{
    geyser_plugin_manager::LoadedGeyserPlugin,
    geyser_plugin_service::{GeyserPluginService, GeyserPluginServiceError},
};

pub fn init_geyser_service(
) -> Result<GeyserPluginService, GeyserPluginServiceError> {
    let grpc_plugin = LoadedGeyserPlugin::new(Box::new(GrpcGeyserPlugin), None);
    let geyser_service = GeyserPluginService::new(&[], vec![grpc_plugin])?;

    Ok(geyser_service)
}
