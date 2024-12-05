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
mod helpers;
mod ledger;
mod metrics;
mod program;
mod rpc;
mod validator;
pub use accounts::*;
pub use geyser_grpc::*;
pub use ledger::*;
pub use metrics::*;
pub use program::*;
pub use rpc::*;
pub use validator::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MagicBlockConfig {
    #[serde(default)]
    pub accounts: AccountsConfig,
    #[serde(default)]
    pub rpc: RpcConfig,
    #[serde(default)]
    pub geyser_grpc: GeyserGrpcConfig,
    #[serde(default)]
    pub validator: ValidatorConfig,
    #[serde(default)]
    pub ledger: LedgerConfig,
    #[serde(default)]
    #[serde(rename = "program")]
    pub programs: Vec<ProgramConfig>,
    #[serde(default)]
    pub metrics: MetricsConfig,
}

impl MagicBlockConfig {
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

    pub fn override_from_envs(&self) -> MagicBlockConfig {
        let mut config = self.clone();

        // -----------------
        // Accounts
        // -----------------
        if let Ok(http) = env::var("ACCOUNTS_REMOTE") {
            if let Ok(ws) = env::var("ACCOUNTS_REMOTE_WS") {
                config.accounts.remote = RemoteConfig::CustomWithWs(
                    Url::parse(&http)
                        .map_err(|err| {
                            panic!(
                                "Invalid 'ACCOUNTS_REMOTE' env var ({:?})",
                                err
                            )
                        })
                        .unwrap(),
                    Url::parse(&ws)
                        .map_err(|err| {
                            panic!(
                                "Invalid 'ACCOUNTS_REMOTE_WS' env var ({:?})",
                                err
                            )
                        })
                        .unwrap(),
                );
            } else {
                config.accounts.remote = RemoteConfig::Custom(
                    Url::parse(&http)
                        .map_err(|err| {
                            panic!(
                                "Invalid 'ACCOUNTS_REMOTE' env var ({:?})",
                                err
                            )
                        })
                        .unwrap(),
                );
            }
        }

        if let Ok(lifecycle) = env::var("ACCOUNTS_LIFECYCLE") {
            config.accounts.lifecycle = lifecycle.parse().unwrap_or_else(|err| {
                panic!(
                    "Failed to parse 'ACCOUNTS_LIFECYCLE' as LifecycleMode: {}: {:?}",
                    lifecycle, err
                )
            })
        }

        if let Ok(frequency_millis) =
            env::var("ACCOUNTS_COMMIT_FREQUENCY_MILLIS")
        {
            config.accounts.commit.frequency_millis = u64::from_str(&frequency_millis)
                .unwrap_or_else(|err| panic!("Failed to parse 'ACCOUNTS_COMMIT_FREQUENCY_MILLIS' as u64: {:?}", err));
        }

        if let Ok(unit_price) = env::var("ACCOUNTS_COMMIT_COMPUTE_UNIT_PRICE") {
            config.accounts.commit.compute_unit_price = u64::from_str(&unit_price)
                .unwrap_or_else(|err| panic!("Failed to parse 'ACCOUNTS_COMMIT_COMPUTE_UNIT_PRICE' as u64: {:?}", err))
        }

        // -----------------
        // RPC
        // -----------------
        if let Ok(addr) = env::var("RPC_ADDR") {
            config.rpc.addr =
                IpAddr::V4(Ipv4Addr::from_str(&addr).unwrap_or_else(|err| {
                    panic!("Failed to parse 'RPC_ADDR' as Ipv4Addr: {:?}", err)
                }));
        }

        if let Ok(port) = env::var("RPC_PORT") {
            config.rpc.port = u16::from_str(&port).unwrap_or_else(|err| {
                panic!("Failed to parse 'RPC_PORT' as u16: {:?}", err)
            });
        }

        // -----------------
        // Geyser GRPC
        // -----------------
        if let Ok(addr) = env::var("GEYSER_GRPC_ADDR") {
            config.geyser_grpc.addr =
                IpAddr::V4(Ipv4Addr::from_str(&addr).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'GEYSER_GRPC_ADDR' as Ipv4Addr: {:?}",
                        err
                    )
                }));
        }

        if let Ok(port) = env::var("GEYSER_GRPC_PORT") {
            config.geyser_grpc.port =
                u16::from_str(&port).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'GEYSER_GRPC_PORT' as u16: {:?}",
                        err
                    )
                });
        }

        // -----------------
        // Validator
        // -----------------
        if let Ok(millis_per_slot) = env::var("VALIDATOR_MILLIS_PER_SLOT") {
            config.validator.millis_per_slot = u64::from_str(&millis_per_slot)
                .unwrap_or_else(|err| panic!("Failed to parse 'VALIDATOR_MILLIS_PER_SLOT' as u64: {:?}", err));
        }

        // -----------------
        // Ledger
        // -----------------
        if let Ok(ledger_reset) = env::var("LEDGER_RESET") {
            config.ledger.reset =
                bool::from_str(&ledger_reset).unwrap_or_else(|err| {
                    panic!("Failed to parse 'LEDGER_RESET' as bool: {:?}", err)
                });
        }
        if let Ok(ledger_path) = env::var("LEDGER_PATH") {
            config.ledger.path = Some(ledger_path);
        }

        // -----------------
        // Metrics
        // -----------------
        if let Ok(enabled) = env::var("METRICS_ENABLED") {
            config.metrics.enabled =
                bool::from_str(&enabled).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'METRICS_ENABLED' as bool: {:?}",
                        err
                    )
                });
        }
        if let Ok(addr) = env::var("METRICS_ADDR") {
            config.metrics.service.addr =
                IpAddr::V4(Ipv4Addr::from_str(&addr).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'METRICS_ADDR' as Ipv4Addr: {:?}",
                        err
                    )
                }));
        }
        if let Ok(port) = env::var("METRICS_PORT") {
            config.metrics.service.port =
                u16::from_str(&port).unwrap_or_else(|err| {
                    panic!("Failed to parse 'METRICS_PORT' as u16: {:?}", err)
                });
        }
        if let Ok(interval) =
            env::var("METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS")
        {
            config.metrics.system_metrics_tick_interval_secs =
                u64::from_str(&interval).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS' as u64: {:?}",
                        err
                    )
                });
        }
        config
    }
}

impl fmt::Display for MagicBlockConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let toml = toml::to_string_pretty(self)
            .unwrap_or("Invalid Config".to_string());
        write!(f, "{}", toml)
    }
}
