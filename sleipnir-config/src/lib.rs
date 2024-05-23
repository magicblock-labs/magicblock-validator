use std::{fmt, fs, path::Path};

use errors::{ConfigError, ConfigResult};
use serde::{Deserialize, Serialize};

mod accounts;
pub mod errors;
mod program;
mod rpc;
mod validator;
pub use accounts::*;
pub use program::*;
pub use rpc::*;
pub use validator::*;

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SleipnirConfig {
    #[serde(default)]
    pub accounts: AccountsConfig,
    #[serde(default)]
    pub rpc: RpcConfig,
    #[serde(default)]
    pub validator: ValidatorConfig,
    #[serde(default)]
    #[serde(rename = "program")]
    pub programs: Vec<ProgramConfig>,
}

impl SleipnirConfig {
    pub fn try_load_from_file(path: &str) -> ConfigResult<Self> {
        let p = Path::new(path);
        let config = fs::read_to_string(p)?;
        let mut config: Self = toml::from_str(&config)?;
        for program in &mut config.programs {
            program.path = p
                .parent()
                .ok_or_else(|| {
                    ConfigError::ConfigPathInvalid(format!(
                        "Config path: '{}' is missing parent dir",
                        path
                    ))
                })?
                .join(&program.path)
                .to_str()
                .ok_or_else(|| {
                    ConfigError::ProgramPathInvalidUnicode(
                        program.id.to_string(),
                        program.path.to_string(),
                    )
                })?
                .to_string();
        }
        Ok(config)
    }
}

impl fmt::Display for SleipnirConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let toml = toml::to_string_pretty(self)
            .unwrap_or("Invalid Config".to_string());
        write!(f, "{}", toml)
    }
}
