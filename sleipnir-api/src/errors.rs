use thiserror::Error;

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("GeyserPluginServiceError error: {0}")]
    GeyserPluginServiceError(#[from] solana_geyser_plugin_manager::geyser_plugin_service::GeyserPluginServiceError),

    #[error("Failed to load programs into bank: {0}")]
    FailedToLoadProgramsIntoBank(String),

    #[error("Failed to initialize JSON RPC service: {0}")]
    FailedToInitJsonRpcService(String),

    #[error("Failed to start JSON RPC service: {0}")]
    FailedToStartJsonRpcService(String),
}
