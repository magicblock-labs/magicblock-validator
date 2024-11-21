use thiserror::Error;

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("GeyserPluginServiceError error: {0}")]
    GeyserPluginServiceError(#[from] solana_geyser_plugin_manager::geyser_plugin_service::GeyserPluginServiceError),

    #[error("Config error: {0}")]
    ConfigError(#[from] sleipnir_config::errors::ConfigError),

    #[error("Pubsub error: {0}")]
    PubsubError(#[from] sleipnir_pubsub::errors::PubsubError),

    #[error("Accounts error: {0}")]
    AccountsError(#[from] sleipnir_accounts::errors::AccountsError),

    #[error("Ledger error: {0}")]
    LedgerError(#[from] sleipnir_ledger::errors::LedgerError),

    #[error("Failed to load programs into bank: {0}")]
    FailedToLoadProgramsIntoBank(String),

    #[error("Failed to initialize JSON RPC service: {0}")]
    FailedToInitJsonRpcService(String),

    #[error("Failed to start JSON RPC service: {0}")]
    FailedToStartJsonRpcService(String),

    #[error("Unable to clean ledger directory at '{0}'")]
    UnableToCleanLedgerDirectory(String),

    #[error("Failed to start metrics service: {0}")]
    FailedToStartMetricsService(std::io::Error),

    #[error("Ledger Path is missing a parent directory: {0}")]
    LedgerPathIsMissingParent(String),

    #[error("Ledger Path has an invalid faucet keypair file: {0} ({1})")]
    LedgerInvalidFaucetKeypair(String, String),

    #[error("Ledger Path is missing a faucet keypair file: {0}")]
    LedgerIsMissingFaucetKeypair(String),

    #[error("Ledger could not write faucet keypair file: {0} ({1})")]
    LedgerCouldNotWriteFaucetKeypair(String, String),

    #[error("Ledger Path has an invalid validator keypair file: {0} ({1})")]
    LedgerInvalidValidatorKeypair(String, String),

    #[error("Ledger Path is missing a validator keypair file: {0}")]
    LedgerIsMissingValidatorKeypair(String),

    #[error("Ledger could not write validator keypair file: {0} ({1})")]
    LedgerCouldNotWriteValidatorKeypair(String, String),

    #[error("Ledger validator keypair '{0}' needs to match the provided one '{1}'")]
    LedgerValidatorKeypairNotMatchingProvidedKeypair(String, String),
}
