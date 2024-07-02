use std::{
    env, fmt, fs,
    net::{IpAddr, Ipv4Addr},
    path::Path,
    str::FromStr,
};

use errors::{ConfigError, ConfigResult};
use serde::{Deserialize, Serialize};
use url::Url;

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

    pub fn override_from_envs(&self) -> SleipnirConfig {
        let mut config = self.clone();

        if let Ok(remote) = env::var("ACCOUNTS.REMOTE") {
            config.accounts.remote = RemoteConfig::Custom(
                Url::parse(&remote)
                    .expect("Failed to parse ACCOUNTS.REMOTE as URL"),
            );
        }
        if let Ok(readonly) = env::var("ACCOUNTS.CLONE.READONLY") {
            config.accounts.clone.readonly = ReadonlyMode::from_str(&readonly)
                .expect(
                    "Failed to parse ACCOUNTS.CLONE.READONLY as ReadonlyMode",
                );
        }
        if let Ok(writable) = env::var("ACCOUNTS.CLONE.WRITABLE") {
            config.accounts.clone.writable = WritableMode::from_str(&writable)
                .expect(
                    "Failed to parse ACCOUNTS.CLONE.WRITABLE as WritableMode",
                );
        }
        if let Ok(frequency_millis) =
            env::var("ACCOUNTS.COMMIT.FREQUENCY_MILLIS")
        {
            config.accounts.commit.frequency_millis = u64::from_str(
                &frequency_millis,
            )
            .expect("Failed to parse ACCOUNTS.COMMIT.FREQUENCY_MILLIS as u64");
        }
        if let Ok(trigger) = env::var("ACCOUNTS.COMMIT.TRIGGER") {
            config.accounts.commit.trigger = bool::from_str(&trigger)
                .expect("Failed to parse ACCOUNTS.COMMIT.TRIGGER as bool");
        }
        if let Ok(unit_price) = env::var("ACCOUNTS.COMMIT.COMPUTE_UNIT_PRICE") {
            config.accounts.commit.compute_unit_price =
                u64::from_str(&unit_price).expect(
                    "Failed to parse ACCOUNTS.COMMIT.COMPUTE_UNIT_PRICE as u64",
                );
        }
        if let Ok(create) = env::var("ACCOUNTS.CREATE") {
            config.accounts.create = bool::from_str(&create)
                .expect("Failed to parse ACCOUNTS.CREATE as bool");
        }
        if let Ok(addr) = env::var("RPC.ADDR") {
            config.rpc.addr = IpAddr::V4(
                Ipv4Addr::from_str(&addr)
                    .expect("Failed to parse RPC.ADDR as Ipv4Addr"),
            );
        }
        if let Ok(port) = env::var("RPC.PORT") {
            config.rpc.port =
                u16::from_str(&port).expect("Failed to parse RPC.PORT as u16");
        }
        if let Ok(addr) = env::var("GEYSER_GRPC.ADDR") {
            config.geyser_grpc.addr = IpAddr::V4(
                Ipv4Addr::from_str(&addr)
                    .expect("Failed to parse GEYSER_GRPC.ADDR as Ipv4Addr"),
            );
        }
        if let Ok(port) = env::var("GEYSER_GRPC.PORT") {
            config.geyser_grpc.port = u16::from_str(&port)
                .expect("Failed to parse GEYSER_GRPC.PORT as u16");
        }
        if let Ok(millis_per_slot) = env::var("VALIDATOR.MILLIS_PER_SLOT") {
            config.validator.millis_per_slot = u64::from_str(&millis_per_slot)
                .expect("Failed to parse VALIDATOR.MILLIS_PER_SLOT as u64");
        }
        config
    }
}

impl fmt::Display for SleipnirConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let toml = toml::to_string_pretty(self)
            .unwrap_or("Invalid Config".to_string());
        write!(f, "{}", toml)
    }
}
