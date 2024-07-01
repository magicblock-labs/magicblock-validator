use std::{fmt, fs, path::Path};

use errors::{ConfigError, ConfigResult};
use serde::{Deserialize, Serialize};

mod accounts;
pub mod errors;
mod geyser_grpc;
mod program;
mod rpc;
mod validator;
pub use accounts::*;
pub use geyser_grpc::*;
pub use program::*;
pub use rpc::*;
pub use validator::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SleipnirConfig {
    #[serde(
        default,
        deserialize_with = "deserialize_accounts_config",
        serialize_with = "serialize_accounts_config"
    )]
    pub accounts: AccountsConfig,
    #[serde(default)]
    pub rpc: RpcConfig,
    #[serde(default)]
    pub geyser_grpc: GeyserGrpcConfig,
    #[serde(default)]
    pub validator: ValidatorConfig,
    #[serde(default)]
    #[serde(rename = "program")]
    pub programs: Vec<ProgramConfig>,
}

fn deserialize_accounts_config<'de, D>(
    deserializer: D,
) -> Result<AccountsConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    AccountsConfig::deserialize(deserializer)
        .map(|accounts_config| {
            if accounts_config.create
                && accounts_config.clone.writable == WritableMode::Delegated
            {
                return Err(serde::de::Error::custom(
                    "AccountsConfig cannot have a [accounts.clone] writable = 'delegated' while allowing new accounts to be created at the same time."
                    .to_string()
                ));
            }
            Ok(accounts_config)
        })?
}

fn serialize_accounts_config<S>(
    accounts_config: &AccountsConfig,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    accounts_config.serialize(serializer)
}

impl SleipnirConfig {
    pub fn try_load_from_file(path: &str) -> ConfigResult<Self> {
        let p = Path::new(path);
        let toml = fs::read_to_string(p)?;
        Self::try_load_from_toml(&toml, Some(p))
    }

    pub fn try_load_from_toml(
        toml: &str,
        config_path: Option<&Path>,
    ) -> ConfigResult<Self> {
        let mut config: Self = toml::from_str(toml)?;
        for program in &mut config.programs {
            // If we know the config path we can resolve relative program paths
            // Otherwise they have to be absolute. However if no config path was
            // provided this usually means that we are provided some default toml
            // config file which doesn't include any program paths.
            if let Some(config_path) = config_path {
                program.path = config_path
                    .parent()
                    .ok_or_else(|| {
                        ConfigError::ConfigPathInvalid(format!(
                            "Config path: '{}' is missing parent dir",
                            config_path.display()
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
                    .to_string()
            }
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
