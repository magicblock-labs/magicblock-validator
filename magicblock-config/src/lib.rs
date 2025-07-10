use std::{
    env, fmt, fs,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    str::FromStr,
};

use clap::Args;
use errors::{ConfigError, ConfigResult};
use isocountry::CountryCode;
use serde::{Deserialize, Serialize};
use solana_sdk::pubkey::Pubkey;
use url::Url;

mod accounts;
mod accounts_db;
mod cli;
pub mod errors;
mod geyser_grpc;
mod helpers;
mod ledger;
mod metrics;
mod program;
mod rpc;
mod validator;
pub use accounts::*;
pub use accounts_db::*;
pub use cli::*;
pub use geyser_grpc::*;
pub use ledger::*;
pub use metrics::*;
pub use program::*;
pub use rpc::*;
pub use validator::*;

#[derive(
    Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize, Args,
)]
#[serde(deny_unknown_fields)]
pub struct EphemeralConfig {
    #[serde(default)]
    #[command(flatten)]
    pub accounts: AccountsConfig,
    #[serde(default)]
    #[command(flatten)]
    pub rpc: RpcConfig,
    #[serde(default)]
    #[command(flatten)]
    pub geyser_grpc: GeyserGrpcConfig,
    #[serde(default)]
    #[command(flatten)]
    pub validator: ValidatorConfig,
    #[serde(default)]
    #[command(flatten)]
    pub ledger: LedgerConfig,
    #[serde(default)]
    #[serde(rename = "program")]
    #[arg(
        long,
        help = "The list of programs to load. Format: <program_id>:<path_to_program_binary>",
        value_parser = program_config_parser,
    )]
    pub programs: Vec<ProgramConfig>,
    #[serde(default)]
    #[command(flatten)]
    pub metrics: MetricsConfig,
}

impl EphemeralConfig {
    pub fn try_load_from_file(path: &PathBuf) -> ConfigResult<Self> {
        let toml = fs::read_to_string(path)?;
        Self::try_load_from_toml(&toml, Some(path))
    }

    pub fn try_load_from_toml(
        toml: &str,
        config_path: Option<&PathBuf>,
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

    pub fn override_from_envs(&self) -> EphemeralConfig {
        let mut config = self.clone();

        // -----------------
        // Accounts
        // -----------------
        config.accounts.remote.url = env::var("REMOTE_URL")
            .ok()
            .and_then(|s| Url::parse(&s).ok());
        config.accounts.remote.ws_url = env::var("REMOTE_WS_URL")
            .ok()
            .and_then(|s| Url::parse(&s).ok().map(|url| vec![url]));
        if config.accounts.remote.url.is_some() {
            if config.accounts.remote.ws_url.is_some() {
                config.accounts.remote.cluster = RemoteCluster::CustomWithWs;
            } else {
                config.accounts.remote.cluster = RemoteCluster::Custom;
            }
        }

        if let Ok(lifecycle) = env::var("ACCOUNTS_LIFECYCLE") {
            config.accounts.lifecycle = lifecycle.parse().unwrap_or_else(|err| {
                panic!("Failed to parse 'ACCOUNTS_LIFECYCLE' as LifecycleMode: {lifecycle}: {err:?}")
            })
        }

        if let Ok(frequency_millis) = env::var("COMMIT_FREQUENCY_MILLIS") {
            config.accounts.commit.frequency_millis = u64::from_str(&frequency_millis)
                .unwrap_or_else(|err| panic!("Failed to parse 'ACCOUNTS_COMMIT_FREQUENCY_MILLIS' as u64: {err:?}"));
        }

        if let Ok(unit_price) = env::var("COMMIT_COMPUTE_UNIT_PRICE") {
            config.accounts.commit.compute_unit_price = u64::from_str(&unit_price)
                .unwrap_or_else(|err| panic!("Failed to parse 'ACCOUNTS_COMMIT_COMPUTE_UNIT_PRICE' as u64: {err:?}"))
        }

        // -----------------
        // RPC
        // -----------------
        if let Ok(addr) = env::var("RPC_ADDR") {
            config.rpc.addr =
                IpAddr::V4(Ipv4Addr::from_str(&addr).unwrap_or_else(|err| {
                    panic!("Failed to parse 'RPC_ADDR' as Ipv4Addr: {err:?}")
                }));
        }

        if let Ok(port) = env::var("RPC_PORT") {
            config.rpc.port = u16::from_str(&port).unwrap_or_else(|err| {
                panic!("Failed to parse 'RPC_PORT' as u16: {err:?}")
            });
        }

        // -----------------
        // Geyser GRPC
        // -----------------
        if let Ok(addr) = env::var("GEYSER_GRPC_ADDR") {
            config.geyser_grpc.addr =
                IpAddr::V4(Ipv4Addr::from_str(&addr).unwrap_or_else(|err| {
                    panic!("Failed to parse 'GEYSER_GRPC_ADDR' as Ipv4Addr: {err:?}")
                }));
        }

        if let Ok(port) = env::var("GEYSER_GRPC_PORT") {
            config.geyser_grpc.port =
                u16::from_str(&port).unwrap_or_else(|err| {
                    panic!("Failed to parse 'GEYSER_GRPC_PORT' as u16: {err:?}")
                });
        }

        // -----------------
        // Validator
        // -----------------
        if let Ok(millis_per_slot) = env::var("VALIDATOR_MILLIS_PER_SLOT") {
            config.validator.millis_per_slot = u64::from_str(&millis_per_slot)
                .unwrap_or_else(|err| panic!("Failed to parse 'VALIDATOR_MILLIS_PER_SLOT' as u64: {err:?}"));
        }

        if let Ok(base_fees) = env::var("VALIDATOR_BASE_FEES") {
            config.validator.base_fees =
                Some(u64::from_str(&base_fees).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'VALIDATOR_BASE_FEES' as u64: {err:?}"
                    )
                }));
        }

        if let Ok(sig_verify) = env::var("VALIDATOR_SIG_VERIFY") {
            config.validator.sigverify = bool::from_str(&sig_verify)
                .unwrap_or_else(|err| {
                    panic!("Failed to parse 'VALIDATOR_SIG_VERIFY' as bool: {err:?}")
                });
        }

        if let Ok(country_code) = env::var("VALIDATOR_COUNTRY_CODE") {
            config.validator.country_code = CountryCode::for_alpha2(&country_code).unwrap_or_else(|err| {
                panic!(
                    "Failed to parse 'VALIDATOR_COUNTRY_CODE' as CountryCode: {err:?}"
                )
            })
        }

        if let Ok(fqdn) = env::var("VALIDATOR_FQDN") {
            config.validator.fqdn = Some(fqdn)
        }

        // -----------------
        // Ledger
        // -----------------
        if let Ok(ledger_reset) = env::var("LEDGER_RESET") {
            config.ledger.reset =
                bool::from_str(&ledger_reset).unwrap_or_else(|err| {
                    panic!("Failed to parse 'LEDGER_RESET' as bool: {err:?}")
                });
        }
        if let Ok(ledger_path) = env::var("LEDGER_PATH") {
            config.ledger.path = Some(ledger_path);
        }
        if let Ok(ledger_path) = env::var("LEDGER_SIZE") {
            config.ledger.size = ledger_path.parse().unwrap_or_else(|err| {
                panic!("Failed to parse 'LEDGER_SIZE' as u64: {err:?}")
            });
        }

        // -----------------
        // Metrics
        // -----------------
        if let Ok(enabled) = env::var("METRICS_ENABLED") {
            config.metrics.enabled =
                bool::from_str(&enabled).unwrap_or_else(|err| {
                    panic!("Failed to parse 'METRICS_ENABLED' as bool: {err:?}")
                });
        }
        if let Ok(addr) = env::var("METRICS_ADDR") {
            config.metrics.service.addr =
                IpAddr::V4(Ipv4Addr::from_str(&addr).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'METRICS_ADDR' as Ipv4Addr: {err:?}"
                    )
                }));
        }
        if let Ok(port) = env::var("METRICS_PORT") {
            config.metrics.service.port =
                u16::from_str(&port).unwrap_or_else(|err| {
                    panic!("Failed to parse 'METRICS_PORT' as u16: {err:?}")
                });
        }
        if let Ok(interval) =
            env::var("METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS")
        {
            config.metrics.system_metrics_tick_interval_secs =
                u64::from_str(&interval).unwrap_or_else(|err| {
                    panic!("Failed to parse 'METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS' as u64: {err:?}")
                });
        }
        config
    }

    pub fn merge(&self, _other: &EphemeralConfig) -> EphemeralConfig {
        self.clone()

        // TODO: Implement this
        // If self differs from the default, use the value from self
        // If other differs from the default but not self, use the value from other
        // If both self and other differ from the default, use the value from self
    }
}

impl fmt::Display for EphemeralConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let toml = toml::to_string_pretty(self)
            .unwrap_or("Invalid Config".to_string());
        write!(f, "{toml}")
    }
}

fn program_config_parser(s: &str) -> Result<ProgramConfig, String> {
    let parts: Vec<String> =
        s.split(':').map(|part| part.to_string()).collect();
    let [id, path] = parts.as_slice() else {
        return Err(format!("Invalid program config: {s}"));
    };
    let id = Pubkey::from_str(id)
        .map_err(|e| format!("Invalid program id {id}: {e}"))?;

    Ok(ProgramConfig {
        id,
        path: path.clone(),
    })
}

#[cfg(test)]
mod tests {
    use solana_sdk::pubkey::Pubkey;

    use super::*;

    #[test]
    fn test_program_config_parser() {
        let config = program_config_parser(
            "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev:path1",
        )
        .unwrap();
        assert_eq!(
            config.id,
            Pubkey::from_str_const(
                "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev"
            )
        );
        assert_eq!(config.path, "path1");
    }
}
