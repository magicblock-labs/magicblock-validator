use solana_geyser_plugin_manager::geyser_plugin_service::{
    GeyserPluginService, GeyserPluginServiceError,
};

pub fn init_geyser_service(
) -> Result<GeyserPluginService, GeyserPluginServiceError> {
    // NOTE: we don't care about confirmed banks since we have a single
    // bank validator
    let geyser_service = GeyserPluginService::new(&[])?;

    // TODO: load builtin plugins

    Ok(geyser_service)
}
