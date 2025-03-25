use thiserror::Error;

pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("Config path error: {0}")]
    ConfigPathInvalid(String),

    #[error("Program with id '{0}' has invalid path '{1}'")]
    ProgramPathInvalidUnicode(String, String),

    #[error("Cannot specify both init_lamports and init_sol")]
    CannotSpecifyBothInitLamportAndInitSol,

    #[error("Failed to estimate purge slot interval")]
    EstimatePurgeSlotError(String),
}
