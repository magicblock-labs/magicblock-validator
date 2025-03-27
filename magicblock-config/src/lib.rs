use std::{
    cmp::min,
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
use magicblock_accounts_db::config::AccountsDbConfig;
pub use metrics::*;
pub use program::*;
pub use rpc::*;
pub use validator::*;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EphemeralConfig {
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

impl EphemeralConfig {
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

    pub fn override_from_envs(&self) -> EphemeralConfig {
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

        if let Ok(init_lamports) = env::var("INIT_LAMPORTS") {
            config.accounts.payer.init_lamports =
                Some(u64::from_str(&init_lamports).unwrap_or_else(|err| {
                    panic!("Failed to parse 'INIT_LAMPORTS' as u64: {:?}", err)
                }));
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

        if let Ok(base_fees) = env::var("VALIDATOR_BASE_FEES") {
            config.validator.base_fees =
                Some(u64::from_str(&base_fees).unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'VALIDATOR_BASE_FEES' as u64: {:?}",
                        err
                    )
                }));
        }

        if let Ok(sig_verify) = env::var("VALIDATOR_SIG_VERIFY") {
            config.validator.sigverify = bool::from_str(&sig_verify)
                .unwrap_or_else(|err| {
                    panic!(
                        "Failed to parse 'VALIDATOR_SIG_VERIFY' as bool: {:?}",
                        err
                    )
                });
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

    /// Calculates how many slots shall pass by for next truncation to happen
    /// We make some assumption here on TPS & size per transaction
    pub fn estimate_purge_slot_interval(&self) -> ConfigResult<u64> {
        // Could be dynamic in the future and fetched from stats.
        const TRANSACTIONS_PER_SECOND: u64 = 50000;
        // Some of the info is duplicated over columns, but mostly a negligible amount
        // So we take solana max transaction size
        const TRANSACTION_MAX_SIZE: u64 = 1232;
        // This implies that we can't delete ledger data past
        // latest MIN_SNAPSHOTS_KEPT snapshot. Has to be at least 1
        const MIN_SNAPSHOTS_KEPT: u16 = 2;

        let millis_per_slot = self.validator.millis_per_slot;
        let transactions_per_slot = {
            let intermediate = (millis_per_slot
                .checked_mul(TRANSACTIONS_PER_SECOND)
                .ok_or(ConfigError::EstimatePurgeSlotError(
                    "millis_per_slot configuration is too high".into(),
                )))?;
            intermediate / 1000
        };
        let size_per_slot = transactions_per_slot * TRANSACTION_MAX_SIZE;

        let AccountsDbConfig {
            max_snapshots,
            snapshot_frequency,
            ..
        } = &self.accounts.db;
        let desired_size = self.ledger.desired_size;

        // Calculate how many snapshot it will take to exceed desired size
        let slots_size = snapshot_frequency.checked_mul(size_per_slot).ok_or(
            ConfigError::EstimatePurgeSlotError(
                "slot_size overflowed. snapshot frequency is too large".into(),
            ),
        )?;

        let num_snapshots_in_desired_size = desired_size
            .checked_div(slots_size)
            .ok_or(ConfigError::EstimatePurgeSlotError(
                "Failed to calculate num_snapshots_in_desired_size".into(),
            ))?;

        // Take min of 2
        let snapshots_kept =
            min(*max_snapshots as u64, num_snapshots_in_desired_size) as u16;

        if snapshots_kept < MIN_SNAPSHOTS_KEPT {
            Err(ConfigError::EstimatePurgeSlotError(
                format!("Desired ledger size is too small. Required snapshots to keep: {}, got: {}",
                        MIN_SNAPSHOTS_KEPT, snapshots_kept)
            ))
        } else {
            Ok(snapshots_kept as u64 * snapshot_frequency)
        }
    }
}

impl fmt::Display for EphemeralConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let toml = toml::to_string_pretty(self)
            .unwrap_or("Invalid Config".to_string());
        write!(f, "{}", toml)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> EphemeralConfig {
        EphemeralConfig {
            validator: ValidatorConfig {
                millis_per_slot: 500,
                ..Default::default()
            },
            accounts: AccountsConfig {
                db: AccountsDbConfig {
                    snapshot_frequency: 100,
                    max_snapshots: 10,
                    ..Default::default()
                },
                ..Default::default()
            },
            ledger: LedgerConfig {
                desired_size: DEFAULT_DESIRED_SIZE,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_estimate_purge_slot_interval() {
        let config = create_test_config();
        let interval = config.estimate_purge_slot_interval().unwrap();

        assert_eq!(interval, 1000);
    }

    #[test]
    fn test_max_snapshots_kept() {
        let mut config = create_test_config();
        config.ledger.desired_size = u64::MAX;
        config.accounts.db.max_snapshots = 5;

        let interval = config.estimate_purge_slot_interval().unwrap();
        assert_eq!(interval, 500);
    }

    #[test]
    fn test_3_snapshots_kept() {
        let mut config = create_test_config();
        config.ledger.desired_size = 10_000_000_000;
        config.accounts.db.max_snapshots = 5;

        let interval = config.estimate_purge_slot_interval().unwrap();
        assert_eq!(interval, 300);
    }

    #[test]
    fn test_overflow_protection() {
        let mut config = create_test_config();
        // Set values that would cause overflow
        config.accounts.db.snapshot_frequency = u64::MAX;
        config.validator.millis_per_slot = u64::MAX;

        let result = config.estimate_purge_slot_interval();
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
           "Failed to estimate purge slot interval: millis_per_slot configuration is too high"
        );
    }
}
